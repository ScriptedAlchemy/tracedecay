use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

const SETTLEMENT_NOT_STARTED: u8 = 0;
const SETTLEMENT_SETTLING: u8 = 1;
const SETTLEMENT_JOINED: u8 = 2;

#[cfg(feature = "hotpath")]
type RetainedDispatchStateMutex<T> = hotpath::wrap::tokio::sync::Mutex<T>;
#[cfg(not(feature = "hotpath"))]
type RetainedDispatchStateMutex<T> = tokio::sync::Mutex<T>;

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

impl DispatchSettlement {
    /// Whether the admitted worker may already have crossed its commit point.
    ///
    /// A worker that never started cannot have produced an effect, so the
    /// client's cancellation or deadline is the whole truth. Once the worker is
    /// settling or has joined without handing back its canonical result, the
    /// effect state is unknown and no client-side terminal may claim otherwise.
    #[hotpath::skip]
    const fn effect_may_have_committed(self) -> bool {
        match self {
            Self::NotStarted => false,
            Self::Settling | Self::Joined => true,
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
    live_cancellable: bool,
    settlement: Arc<DispatchExecutionSettlement>,
    _gauge: ActiveDispatchGaugeGuard,
}

struct ActiveDispatchGaugeGuard;

impl ActiveDispatchGaugeGuard {
    fn enter() -> Self {
        hotpath::gauge!("mcp.server.dispatch.active").inc(1_u64);
        hotpath::gauge!("mcp.server.dispatch.admitted_total").inc(1_u64);
        Self
    }
}

impl Drop for ActiveDispatchGaugeGuard {
    fn drop(&mut self) {
        hotpath::gauge!("mcp.server.dispatch.active").dec(1_u64);
        hotpath::gauge!("mcp.server.dispatch.settled_total").inc(1_u64);
    }
}

struct DispatchAdmissionWaitGuard;

impl DispatchAdmissionWaitGuard {
    fn enter() -> Self {
        hotpath::gauge!("mcp.server.dispatch.admission_waiters").inc(1_u64);
        Self
    }
}

impl Drop for DispatchAdmissionWaitGuard {
    fn drop(&mut self) {
        hotpath::gauge!("mcp.server.dispatch.admission_waiters").dec(1_u64);
    }
}

struct DispatchCapacityLease {
    active_slots: Arc<AtomicUsize>,
}

impl Drop for DispatchCapacityLease {
    fn drop(&mut self) {
        self.active_slots.fetch_sub(1, Ordering::AcqRel);
    }
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
    active_slots: Arc<AtomicUsize>,
    state: RetainedDispatchStateMutex<RetainedDispatchState>,
    #[cfg(test)]
    retained_spawn_count: AtomicUsize,
    #[cfg(test)]
    connection_owned_count: AtomicUsize,
}

impl RetainedDispatchRegistry {
    pub(super) fn new() -> Self {
        Self::new_with_capacity(dispatch_capacity_for_host())
    }

    fn new_with_capacity(capacity: usize) -> Self {
        Self {
            accepting: AtomicBool::new(true),
            capacity,
            active_slots: Arc::new(AtomicUsize::new(0)),
            state: hotpath::mutex!(
                tokio::sync::Mutex::new(RetainedDispatchState {
                    tasks: tokio::task::JoinSet::new(),
                    active: HashMap::new(),
                }),
                label = "mcp.server.dispatch.registry"
            ),
            #[cfg(test)]
            retained_spawn_count: AtomicUsize::new(0),
            #[cfg(test)]
            connection_owned_count: AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    fn new_with_capacity_for_test(capacity: usize) -> Self {
        Self::new_with_capacity(capacity)
    }

    #[hotpath::measure(label = "mcp.server.dispatch.admission")]
    fn acquire_capacity(&self) -> Result<DispatchCapacityLease> {
        loop {
            if !self.accepting.load(Ordering::Acquire) {
                hotpath::gauge!("mcp.server.dispatch.refused_shutdown_total").inc(1_u64);
                return Err(dispatch_shutdown_error());
            }
            let active = self.active_slots.load(Ordering::Acquire);
            if active >= self.capacity {
                hotpath::gauge!("mcp.server.dispatch.refused_saturated_total").inc(1_u64);
                return Err(dispatch_saturated_error());
            }
            if self
                .active_slots
                .compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let lease = DispatchCapacityLease {
                    active_slots: Arc::clone(&self.active_slots),
                };
                if self.accepting.load(Ordering::Acquire) {
                    return Ok(lease);
                }
                drop(lease);
                hotpath::gauge!("mcp.server.dispatch.refused_shutdown_total").inc(1_u64);
                return Err(dispatch_shutdown_error());
            }
        }
    }

    async fn spawn<T, F>(
        &self,
        cancellation: tracedecay_application::CancellationSignal,
        live_cancellable: bool,
        future: F,
    ) -> Result<(
        tokio::sync::oneshot::Receiver<Result<T>>,
        Arc<DispatchExecutionSettlement>,
    )>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let capacity_lease = self.acquire_capacity()?;
        let admission_wait = DispatchAdmissionWaitGuard::enter();
        let mut state = self.state.lock().await;
        drop(admission_wait);
        Self::reap_finished(&mut state);
        if !self.accepting.load(Ordering::Acquire) {
            // Admission refusals are the signal a saturation diagnosis needs;
            // count them alongside the admitted/settled lifecycle gauges.
            hotpath::gauge!("mcp.server.dispatch.refused_shutdown_total").inc(1_u64);
            return Err(dispatch_shutdown_error());
        }

        let settlement = Arc::new(DispatchExecutionSettlement::not_started());
        let worker_settlement = Arc::clone(&settlement);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        #[cfg(test)]
        self.retained_spawn_count.fetch_add(1, Ordering::AcqRel);
        let task = state.tasks.spawn(async move {
            let _capacity_lease = capacity_lease;
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
                live_cancellable,
                settlement: Arc::clone(&settlement),
                _gauge: ActiveDispatchGaugeGuard::enter(),
            },
        );
        Ok((receiver, settlement))
    }

    #[cfg(test)]
    #[hotpath::skip]
    async fn active_count_for_test(&self) -> usize {
        self.state.lock().await.active.len()
    }

    #[cfg(test)]
    pub(super) fn retained_spawn_count_for_test(&self) -> usize {
        self.retained_spawn_count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn connection_owned_count_for_test(&self) -> usize {
        self.connection_owned_count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn active_slot_count_for_test(&self) -> usize {
        self.active_slots.load(Ordering::Acquire)
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

    #[hotpath::skip]
    pub(super) async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        self.accepting.store(false, Ordering::Release);
        let requested_at = tracedecay_application::clock::now_micros();
        for active in state.active.values() {
            if active.live_cancellable {
                let _ = active.cancellation.cancel(requested_at);
            }
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
    /// Signalled on every cancellation registration so a cancellation
    /// notification that raced route resolution can sleep until the request
    /// registers instead of polling the map.
    cancellation_registered: tokio::sync::Notify,
    registry: RetainedDispatchRegistry,
    server: std::sync::Weak<super::McpServer>,
}

impl RetainedDispatchAuthority {
    pub(super) fn new(server: std::sync::Weak<super::McpServer>) -> Self {
        Self {
            cancellations: std::sync::Mutex::new(HashMap::new()),
            cancellation_registered: tokio::sync::Notify::new(),
            registry: RetainedDispatchRegistry::new(),
            server,
        }
    }

    pub(super) fn cancellations(
        &self,
    ) -> &std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>> {
        &self.cancellations
    }

    pub(super) fn register_cancellation(
        &self,
        request_id: String,
        cancellation: tracedecay_application::CancellationSignal,
    ) {
        super::requests::recover_lock(&self.cancellations).insert(request_id, cancellation);
        self.cancellation_registered.notify_waiters();
    }

    pub(super) fn cancellation_registered(&self) -> &tokio::sync::Notify {
        &self.cancellation_registered
    }

    pub(super) fn registry(&self) -> &RetainedDispatchRegistry {
        &self.registry
    }

    pub(super) fn server(&self) -> std::sync::Weak<super::McpServer> {
        self.server.clone()
    }

    #[hotpath::skip]
    pub(super) async fn shutdown(&self) {
        let requested_at = tracedecay_application::clock::now_micros();
        for cancellation in super::requests::recover_lock(&self.cancellations).values() {
            let _ = cancellation.cancel(requested_at);
        }
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
    live_cancellable: bool,
    canonical_effect_settlement: bool,
}

impl super::McpServer {
    pub(super) fn prepare_dispatch_control<'a>(
        &'a self,
        id: &serde_json::Value,
        tool_name: &str,
        memory_request_scope: &str,
        pre_cancelled: bool,
        caller_deadline: Option<tracedecay_application::Deadline>,
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
            self.dispatch_authority
                .register_cancellation(request_id.clone(), cancellation.clone());
        }
        let registration = ApplicationCancellationRegistration::new(
            self.dispatch_authority.cancellations(),
            registered_request_id,
        );
        let application_surface = ApplicationSurfaceOperation::from_tool_name(tool_name);
        let source_edit = super::requests::is_source_edit_tool(tool_name);
        let controlled_read = super::requests::is_controlled_read_tool(tool_name);
        let ceiling = crate::mcp::tools::binding::canonical_tool_dispatch_ceiling(tool_name)
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not resolve MCP dispatch deadline: {error}"),
            })?;
        let carried_horizon = if application_surface.is_some() {
            i64::try_from(ceiling.as_micros()).ok()
        } else {
            super::requests::dispatch_deadline_horizon_micros(controlled_read || source_edit)
        };
        let carried_deadline = carried_horizon.and_then(|horizon| {
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                super::requests::mcp_now_micros().0.saturating_add(horizon),
            ))
            .ok()
        });
        let ceiling_micros =
            i64::try_from(ceiling.as_micros()).map_err(|_| TraceDecayError::Config {
                message: "MCP dispatch ceiling exceeds the domain clock".to_owned(),
            })?;
        let ceiling_deadline = tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
            super::requests::mcp_now_micros()
                .0
                .saturating_add(ceiling_micros),
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid MCP dispatch deadline: {error}"),
        })?;
        // A caller that named its own deadline is the authority on its budget,
        // up to the tool's ceiling. Without this, every `tools/call` — the CLI
        // compatibility route and every MCP host — was served on the ceiling no
        // matter what the caller asked for, so a caller-visible deadline could
        // never reach admission or settlement and the typed terminals it exists
        // to produce were unreachable through this transport. An already
        // elapsed caller deadline is kept as-is on purpose: the honest answer
        // to it is the typed timeout, not a silently widened budget.
        let deadline = match caller_deadline {
            Some(caller) if caller.expires_at <= ceiling_deadline.expires_at => caller,
            _ => match carried_deadline {
                Some(deadline)
                    if tracedecay_daemon_protocol::deadline_remaining(&deadline)
                        .is_some_and(|remaining| remaining <= ceiling) =>
                {
                    deadline
                }
                _ => ceiling_deadline,
            },
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
        let remaining = tracedecay_daemon_protocol::deadline_remaining(&deadline)
            .ok_or_else(|| dispatch_deadline_error(&tool_name, DispatchSettlement::NotStarted))?;
        let deadline_at = tokio::time::Instant::now()
            .checked_add(remaining)
            .ok_or_else(|| TraceDecayError::Config {
                message: "MCP dispatch deadline cannot be represented by the runtime clock"
                    .to_owned(),
            })?;
        Ok(Self {
            live_cancellable: crate::mcp::tools::binding::tool_supports_live_cancellation(
                &tool_name,
            ),
            canonical_effect_settlement:
                crate::mcp::tools::binding::tool_requires_canonical_effect_settlement(&tool_name),
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

    pub(super) fn permits_connection_owned_execution(&self) -> bool {
        !tool_carries_effect(&self.tool_name) && !self.canonical_effect_settlement
    }

    #[hotpath::measure(label = "mcp.server.dispatch.settlement", future = true)]
    pub(super) async fn run_connection_owned<T, F>(
        &self,
        registry: &RetainedDispatchRegistry,
        future: F,
    ) -> RetainedDispatchOutcome<T>
    where
        F: Future<Output = Result<T>> + Send,
    {
        let cancelled_before_admission = self.cancellation.is_cancelled();
        if cancelled_before_admission && !self.live_cancellable {
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

        let _capacity_lease = match registry.acquire_capacity() {
            Ok(lease) => lease,
            Err(error) => return RetainedDispatchOutcome::failed(error),
        };
        #[cfg(test)]
        registry
            .connection_owned_count
            .fetch_add(1, Ordering::AcqRel);
        let settlement = Arc::new(DispatchExecutionSettlement::not_started());
        settlement.mark_settling();
        let deadline = tokio::time::sleep_until(self.deadline_at);
        let cancellation =
            tracedecay_daemon_protocol::wait_for_cancellation(self.cancellation.clone());
        tokio::pin!(future);
        tokio::pin!(deadline);
        tokio::pin!(cancellation);

        let outcome = tokio::select! {
            biased;
            () = &mut cancellation, if self.live_cancellable => {
                Err(DispatchFailure::new(dispatch_cancelled_error(
                    &self.tool_name,
                    if cancelled_before_admission {
                        DispatchSettlement::NotStarted
                    } else {
                        settlement.snapshot()
                    },
                )))
            }
            () = &mut deadline => {
                let _ = self
                    .cancellation
                    .cancel(tracedecay_application::clock::now_micros());
                Err(DispatchFailure::new(dispatch_deadline_error(
                    &self.tool_name,
                    settlement.snapshot(),
                )))
            }
            output = &mut future => output.map_err(DispatchFailure::new),
        };
        settlement.mark_joined();
        RetainedDispatchOutcome {
            result: outcome,
            settlement,
        }
    }

    #[hotpath::measure(label = "mcp.server.dispatch.settlement", future = true)]
    pub(super) async fn run_retained<T, F>(
        &self,
        registry: &RetainedDispatchRegistry,
        future: F,
    ) -> RetainedDispatchOutcome<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        // A pre-cancelled cooperative dispatch is still admitted: its
        // invocation authority observes the already-cancelled signal itself —
        // that is the cooperative contract — so the settlement it records is
        // the authoritative one, and a cancellation that raced request
        // registration still reaches the application executor. Only a tool
        // with no live cancellation observer keeps the pre-admission refusal,
        // because nothing downstream would ever consult the signal.
        let cancelled_before_admission = self.cancellation.is_cancelled();
        if cancelled_before_admission && !self.live_cancellable {
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

        let (mut result, settlement) = match registry
            .spawn(self.cancellation.clone(), self.live_cancellable, future)
            .await
        {
            Ok(admitted) => admitted,
            Err(error) => return RetainedDispatchOutcome::failed(error),
        };
        let deadline = tokio::time::sleep_until(self.deadline_at);
        let cancellation =
            tracedecay_daemon_protocol::wait_for_cancellation(self.cancellation.clone());
        tokio::pin!(deadline);
        tokio::pin!(cancellation);

        let outcome = tokio::select! {
            biased;
            () = &mut cancellation, if self.live_cancellable => {
                Err(DispatchFailure::new(dispatch_cancelled_error(
                    &self.tool_name,
                    // A signal cancelled before this worker was admitted has
                    // already won the commit compare-and-swap: the effect's
                    // commit point is unreachable no matter how far the
                    // admitted worker has raced, so the plain pre-admission
                    // terminal stays truthful and effect-unknown is reserved
                    // for cancellations that arrived after admission.
                    if cancelled_before_admission {
                        DispatchSettlement::NotStarted
                    } else {
                        settlement.snapshot()
                    },
                )))
            }
            () = &mut deadline => {
                // An effect worker that is already settling is *producing the
                // authoritative answer to this very deadline*: the retained
                // owners report a `PartialEffect` carrying the committed
                // receipt and a Reconcile-only legal action when their budget
                // expires after the commit point. Abandoning it here replaced
                // that typed terminal with `tool_dispatch_effect_unknown` —
                // telling the caller to "inspect the daemon receipt" while the
                // daemon was holding the receipt and about to hand it over.
                // Effect-unknown is for a result that is genuinely
                // unavailable, which is exactly what this branch's own
                // contract says; awaiting the canonical result is what makes
                // that true.
                if self.canonical_effect_settlement
                    || (settlement.snapshot().effect_may_have_committed()
                        && tool_carries_effect(&self.tool_name))
                {
                    receive_canonical_result(&mut result).await
                } else if self
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

#[hotpath::measure(label = "mcp.server.dispatch.canonical_wait", future = true)]
async fn receive_canonical_result<T>(
    result: &mut tokio::sync::oneshot::Receiver<Result<T>>,
) -> std::result::Result<T, DispatchFailure> {
    received_result(result.await)
}

/// The one terminal an admitted-but-unsettled dispatch is allowed to report.
///
/// After admission the daemon's operation/effect receipt is
/// authoritative, and if an effect may have crossed its commit point then
/// cancellation or a client timeout cannot replace effect-unknown state. The
/// worker settlement is what decides which of those two worlds the caller is
/// in, so it selects the reason code rather than only decorating the message.
fn effect_unknown_error(
    tool_name: &str,
    settlement: DispatchSettlement,
    cause: &str,
) -> TraceDecayError {
    hotpath::gauge!("mcp.server.dispatch.effect_unknown_total").inc(1_u64);
    TraceDecayError::project_route(
        "tool_dispatch_effect_unknown",
        false,
        format!(
            "tool '{tool_name}' stopped being awaited after {cause}, but its admitted worker had already started; worker settlement is {settlement:?} and its effect state is unknown. Inspect the daemon receipt before retrying."
        ),
    )
}

/// Whether abandoning this tool mid-flight can leave state behind.
///
/// A read that stops being awaited leaves nothing to reconcile, so it keeps the
/// plain cancelled/deadline terminal. Only a tool whose dispatch contract
/// declares an effect can reach effect-unknown.
fn tool_carries_effect(tool_name: &str) -> bool {
    crate::mcp::tools::binding::mcp_dispatch_contract(tool_name)
        .is_ok_and(|contract| !contract.read_only())
}

pub(super) fn dispatch_cancelled_error(
    tool_name: &str,
    settlement: DispatchSettlement,
) -> TraceDecayError {
    if settlement.effect_may_have_committed() && tool_carries_effect(tool_name) {
        return effect_unknown_error(tool_name, settlement, "cancellation");
    }
    hotpath::gauge!("mcp.server.dispatch.cancelled_total").inc(1_u64);
    TraceDecayError::project_route(
        "tool_dispatch_cancelled",
        true,
        format!(
            "tool '{tool_name}' was cancelled before commit; worker settlement is {settlement:?}"
        ),
    )
}

fn dispatch_deadline_error(tool_name: &str, settlement: DispatchSettlement) -> TraceDecayError {
    if settlement.effect_may_have_committed() && tool_carries_effect(tool_name) {
        return effect_unknown_error(tool_name, settlement, "its absolute deadline");
    }
    hotpath::gauge!("mcp.server.dispatch.deadline_total").inc(1_u64);
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

fn dispatch_saturated_error() -> TraceDecayError {
    TraceDecayError::project_route(
        "tool_dispatch_saturated",
        true,
        "MCP retained dispatch capacity is exhausted",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

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
    async fn live_cancellation_before_commit_returns_cancelled_while_the_worker_remains_owned() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.before-commit")
                .expect("cancellation");
        let control = DispatchControl::new(
            "tracedecay_search",
            deadline_after(std::time::Duration::from_mins(1)),
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
                    Ok::<_, tracedecay_domain::errors::TraceDecayError>("not committed")
                })
                .await
        });

        worker_started.notified().await;
        assert_eq!(registry.active_count_for_test().await, 1);
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
        assert_eq!(registry.active_count_for_test().await, 0);
        assert_eq!(settlement.snapshot(), DispatchSettlement::Joined);
    }

    #[tokio::test]
    async fn non_cancellable_effect_returns_the_canonical_result_after_admission() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.after-commit")
                .expect("cancellation");
        let control = DispatchControl::new(
            "tracedecay_configuration_set",
            deadline_after(std::time::Duration::from_mins(1)),
            cancellation.clone(),
        )
        .expect("control");
        let effect_started = Arc::new(tokio::sync::Notify::new());
        let worker_release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&effect_started);
        let release = Arc::clone(&worker_release);
        let runner_registry = Arc::clone(&registry);
        let runner = tokio::spawn(async move {
            control
                .run_retained(&runner_registry, async move {
                    started.notify_one();
                    release.notified().await;
                    Ok::<_, tracedecay_domain::errors::TraceDecayError>("committed")
                })
                .await
        });

        effect_started.notified().await;
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        assert!(
            !runner.is_finished(),
            "an admitted non-cancellable effect owns canonical settlement"
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

    /// A cooperative dispatch cancelled before admission is still admitted so
    /// its invocation authority observes the signal and records the
    /// authoritative settlement — a cancellation that raced request
    /// registration must still reach the application executor — while the
    /// caller reads the plain pre-admission cancelled terminal.
    #[tokio::test]
    async fn pre_cancelled_cooperative_dispatch_still_reaches_its_worker() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.pre-admission-cooperative")
                .expect("cancellation");
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        let control = DispatchControl::new(
            "tracedecay_search",
            deadline_after(std::time::Duration::from_mins(1)),
            cancellation,
        )
        .expect("control");
        let worker_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_flag = Arc::clone(&worker_ran);
        let outcome = control
            .run_retained(&registry, async move {
                worker_flag.store(true, std::sync::atomic::Ordering::Release);
                Ok::<_, tracedecay_domain::errors::TraceDecayError>("authority settled")
            })
            .await;
        let failure = outcome
            .result
            .expect_err("a pre-cancelled dispatch reports its typed cancelled terminal");
        assert_eq!(
            failure.project_route_context().map(|context| context.0),
            Some("tool_dispatch_cancelled"),
            "pre-admission cancellation keeps the plain cancelled terminal"
        );
        assert_eq!(
            failure.project_route_context().map(|context| context.1),
            Some(true),
            "nothing committed, so the pre-admission terminal stays retryable"
        );
        registry.shutdown().await;
        assert!(
            worker_ran.load(std::sync::atomic::Ordering::Acquire),
            "the admitted worker must run so the invocation authority settles it"
        );
    }

    /// A pre-cancelled effect dispatch cannot degrade to effect-unknown: the
    /// cancellation won the commit compare-and-swap before admission, so the
    /// commit point is unreachable however far the admitted worker raced.
    #[tokio::test]
    async fn pre_cancelled_cooperative_effect_reports_cancelled_not_effect_unknown() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.pre-admission-effect")
                .expect("cancellation");
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        let control = DispatchControl::new(
            "tracedecay_str_replace",
            deadline_after(std::time::Duration::from_mins(1)),
            cancellation,
        )
        .expect("control");
        let outcome = control
            .run_retained(&registry, async move {
                Ok::<_, tracedecay_domain::errors::TraceDecayError>("commit unreachable")
            })
            .await;
        let failure = outcome
            .result
            .expect_err("a pre-cancelled effect dispatch cannot report success");
        assert_eq!(
            failure.project_route_context().map(|context| context.0),
            Some("tool_dispatch_cancelled"),
            "the commit CAS was already lost, so effect-unknown would be untruthful"
        );
        registry.shutdown().await;
    }

    /// A tool with no live cancellation observer keeps the pre-admission
    /// refusal: nothing downstream would ever consult the signal, so admitting
    /// the worker would only run work whose answer is already decided.
    #[tokio::test]
    async fn pre_cancelled_non_cooperative_dispatch_is_refused_before_admission() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.pre-admission-refused")
                .expect("cancellation");
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        let control = DispatchControl::new(
            "tracedecay_configuration_set",
            deadline_after(std::time::Duration::from_mins(1)),
            cancellation,
        )
        .expect("control");
        let worker_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_flag = Arc::clone(&worker_ran);
        let outcome = control
            .run_retained(&registry, async move {
                worker_flag.store(true, std::sync::atomic::Ordering::Release);
                Ok::<_, tracedecay_domain::errors::TraceDecayError>("never admitted")
            })
            .await;
        assert_eq!(outcome.settlement(), DispatchSettlement::NotStarted);
        let failure = outcome
            .result
            .expect_err("a pre-cancelled non-cooperative dispatch is refused");
        assert_eq!(
            failure.project_route_context().map(|context| context.0),
            Some("tool_dispatch_cancelled")
        );
        registry.shutdown().await;
        assert!(
            !worker_ran.load(std::sync::atomic::Ordering::Acquire),
            "no settlement authority exists, so the worker must never be admitted"
        );
    }

    /// A live-cancellable effect tool whose worker was already admitted cannot
    /// answer "cancelled": the effect may have crossed its commit point, and
    /// the caller has to inspect the daemon receipt instead of retrying.
    #[tokio::test]
    async fn live_cancellation_after_admission_reports_effect_unknown_for_an_effect_tool() {
        let registry = Arc::new(RetainedDispatchRegistry::new());
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.effect-in-flight")
                .expect("cancellation");
        let control = DispatchControl::new(
            "tracedecay_str_replace",
            deadline_after(std::time::Duration::from_mins(1)),
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
                    Ok::<_, tracedecay_domain::errors::TraceDecayError>(
                        "effect state is the daemon's to say",
                    )
                })
                .await
        });

        worker_started.notified().await;
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        let abandoned = runner.await.expect("dispatch runner");
        let settlement = Arc::clone(&abandoned.settlement);
        let failure = abandoned
            .result
            .expect_err("an abandoned effect worker cannot report success");
        assert_eq!(
            failure.project_route_context().map(|context| context.0),
            Some("tool_dispatch_effect_unknown"),
            "an admitted effect worker may have crossed its commit point"
        );
        assert_eq!(
            failure.project_route_context().map(|context| context.1),
            Some(false),
            "effect-unknown is never a safe blind retry"
        );
        assert_eq!(settlement.snapshot(), DispatchSettlement::Settling);

        worker_release.notify_one();
        registry.shutdown().await;
        assert_eq!(settlement.snapshot(), DispatchSettlement::Joined);
    }

    #[tokio::test]
    async fn inline_read_capacity_refuses_the_next_retained_effect() {
        let registry = Arc::new(RetainedDispatchRegistry::new_with_capacity_for_test(1));
        let read_control = DispatchControl::new(
            "tracedecay_status",
            deadline_after(std::time::Duration::from_mins(1)),
            tracedecay_application::CancellationSignal::active("capacity.inline-read")
                .expect("read cancellation"),
        )
        .expect("read control");
        let read_started = Arc::new(tokio::sync::Notify::new());
        let read_release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&read_started);
        let release = Arc::clone(&read_release);
        let read_registry = Arc::clone(&registry);
        let read = tokio::spawn(async move {
            read_control
                .run_connection_owned(&read_registry, async move {
                    started.notify_one();
                    release.notified().await;
                    Ok::<_, tracedecay_domain::errors::TraceDecayError>("read")
                })
                .await
        });
        read_started.notified().await;
        assert_eq!(registry.active_slot_count_for_test(), 1);

        let effect_control = DispatchControl::new(
            "tracedecay_configuration_set",
            deadline_after(std::time::Duration::from_mins(1)),
            tracedecay_application::CancellationSignal::active("capacity.retained-effect")
                .expect("effect cancellation"),
        )
        .expect("effect control");
        let refused = effect_control
            .run_retained(&registry, async {
                Ok::<_, tracedecay_domain::errors::TraceDecayError>("effect")
            })
            .await;
        assert_eq!(
            refused
                .result
                .expect_err("retained effect must be refused")
                .project_route_context()
                .map(|context| context.0),
            Some("tool_dispatch_saturated")
        );

        read_release.notify_one();
        assert_eq!(read.await.expect("join read").result.expect("read"), "read");
        assert_eq!(registry.active_slot_count_for_test(), 0);
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn retained_effect_capacity_refuses_the_next_inline_read() {
        let registry = Arc::new(RetainedDispatchRegistry::new_with_capacity_for_test(1));
        let effect_control = DispatchControl::new(
            "tracedecay_configuration_set",
            deadline_after(std::time::Duration::from_mins(1)),
            tracedecay_application::CancellationSignal::active("capacity.retained-owner")
                .expect("effect cancellation"),
        )
        .expect("effect control");
        let effect_started = Arc::new(tokio::sync::Notify::new());
        let effect_release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::clone(&effect_started);
        let release = Arc::clone(&effect_release);
        let effect_registry = Arc::clone(&registry);
        let effect = tokio::spawn(async move {
            effect_control
                .run_retained(&effect_registry, async move {
                    started.notify_one();
                    release.notified().await;
                    Ok::<_, tracedecay_domain::errors::TraceDecayError>("effect")
                })
                .await
        });
        effect_started.notified().await;
        assert_eq!(registry.active_slot_count_for_test(), 1);

        let read_control = DispatchControl::new(
            "tracedecay_status",
            deadline_after(std::time::Duration::from_mins(1)),
            tracedecay_application::CancellationSignal::active("capacity.inline-refused")
                .expect("read cancellation"),
        )
        .expect("read control");
        let refused = read_control
            .run_connection_owned(&registry, async {
                Ok::<_, tracedecay_domain::errors::TraceDecayError>("read")
            })
            .await;
        assert_eq!(
            refused
                .result
                .expect_err("inline read must be refused")
                .project_route_context()
                .map(|context| context.0),
            Some("tool_dispatch_saturated")
        );

        effect_release.notify_one();
        assert_eq!(
            effect.await.expect("join effect").result.expect("effect"),
            "effect"
        );
        assert_eq!(registry.active_slot_count_for_test(), 0);
        registry.shutdown().await;
    }

    struct FutureDropObserver(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for FutureDropObserver {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn connection_owned_cancellation_drops_the_read_and_joins_settlement() {
        let registry = RetainedDispatchRegistry::new();
        let cancellation = tracedecay_application::CancellationSignal::active("inline.cancel-drop")
            .expect("cancellation");
        let control = DispatchControl::new(
            "tracedecay_search",
            deadline_after(std::time::Duration::from_mins(1)),
            cancellation.clone(),
        )
        .expect("control");
        let started = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::clone(&started);
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drop_observer = Arc::clone(&dropped);
        let dispatch = async move {
            let _drop_observer = FutureDropObserver(drop_observer);
            entered.notify_one();
            std::future::pending::<tracedecay_domain::errors::Result<&'static str>>().await
        };
        let runner =
            tokio::spawn(async move { control.run_connection_owned(&registry, dispatch).await });

        started.notified().await;
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        let cancelled = runner.await.expect("join inline dispatch");
        assert_eq!(
            cancelled
                .result
                .as_ref()
                .expect_err("cancellation must win")
                .project_route_context()
                .map(|context| context.0),
            Some("tool_dispatch_cancelled")
        );
        assert_eq!(cancelled.settlement(), DispatchSettlement::Joined);
        assert!(
            dropped.load(Ordering::Acquire),
            "connection-owned reads have no post-cancel owner"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn connection_owned_deadline_drops_the_read_and_joins_settlement() {
        let registry = RetainedDispatchRegistry::new();
        let control = DispatchControl::new(
            "tracedecay_status",
            deadline_after(std::time::Duration::from_secs(1)),
            tracedecay_application::CancellationSignal::active("inline.deadline-drop")
                .expect("cancellation"),
        )
        .expect("control");
        let started = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::clone(&started);
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drop_observer = Arc::clone(&dropped);
        let dispatch = async move {
            let _drop_observer = FutureDropObserver(drop_observer);
            entered.notify_one();
            std::future::pending::<tracedecay_domain::errors::Result<&'static str>>().await
        };
        let runner =
            tokio::spawn(async move { control.run_connection_owned(&registry, dispatch).await });

        started.notified().await;
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        let timed_out = runner.await.expect("join inline dispatch");
        assert_eq!(
            timed_out
                .result
                .as_ref()
                .expect_err("deadline must win")
                .project_route_context()
                .map(|context| context.0),
            Some("tool_dispatch_deadline_exceeded")
        );
        assert_eq!(timed_out.settlement(), DispatchSettlement::Joined);
        assert!(
            dropped.load(Ordering::Acquire),
            "connection-owned reads have no post-deadline owner"
        );
    }
}
