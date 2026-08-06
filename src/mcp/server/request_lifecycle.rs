//! One server-owned lifecycle authority for MCP tool requests.
//!
//! Every stage shares the enqueue-time deadline and cancellation signal.
//! Unsettled join-required workers transfer to the server's single reaper.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::errors::{Result, TraceDecayError};

mod policy;
pub(crate) use policy::{McpRequestStart, McpToolDispatchStage, McpToolLifecyclePolicy};

const MAX_PENDING_CANCELLATIONS: usize = 128;
const PENDING_CANCELLATION_RETENTION: Duration = Duration::from_mins(10);
const MAX_ACTIVE_REQUESTS: usize = 64;
const MAX_PENDING_WORKER_SETTLEMENTS: usize = 32;
const MAX_RETAINED_WORKER_SETTLEMENTS: usize = 128;

#[derive(Clone)]
pub(crate) struct McpRequestRegistry {
    inner: Arc<McpRequestRegistryInner>,
}

struct McpRequestRegistryInner {
    next_registration: AtomicU64,
    requests: Mutex<McpRequestRegistryState>,
    admission_permits: Arc<Semaphore>,
    reaper: McpWorkerSettlementReaper,
}

#[derive(Default)]
struct McpRequestRegistryState {
    active: HashMap<String, ActiveMcpRequest>,
    pending_cancellations: BTreeMap<String, tokio::time::Instant>,
}

struct ActiveMcpRequest {
    registration: u64,
    cancellation: tracedecay_application::CancellationSignal,
    termination: Arc<AtomicU8>,
    externally_cancellable: bool,
}

impl Default for McpRequestRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl McpRequestRegistry {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(McpRequestRegistryInner {
                next_registration: AtomicU64::new(1),
                requests: Mutex::new(McpRequestRegistryState::default()),
                admission_permits: Arc::new(Semaphore::new(MAX_ACTIVE_REQUESTS)),
                reaper: McpWorkerSettlementReaper::new(),
            }),
        }
    }

    pub(crate) fn admit(
        &self,
        request_key: &str,
        tool_name: &str,
        started: McpRequestStart,
        policy: McpToolLifecyclePolicy,
    ) -> Result<McpToolDispatchControl> {
        let response_deadline_at = started
            .runtime
            .checked_add(policy.maximum_duration)
            .ok_or_else(|| TraceDecayError::Config {
                message: "MCP request deadline cannot be represented by the runtime clock"
                    .to_owned(),
            })?;
        let deadline_at = started
            .runtime
            .checked_add(policy.execution_duration())
            .ok_or_else(|| TraceDecayError::Config {
                message: "MCP execution deadline cannot be represented by the runtime clock"
                    .to_owned(),
            })?;
        if tokio::time::Instant::now() >= deadline_at {
            return Err(dispatch_error(
                tool_name,
                policy.maximum_duration,
                "tool_dispatch_deadline_exceeded",
                McpToolDispatchStage::QueueAdmission,
                true,
            ));
        }
        let admission_permit = Arc::clone(&self.inner.admission_permits)
            .try_acquire_owned()
            .map_err(|_| {
                TraceDecayError::mcp_tool_dispatch(
                    "tool_dispatch_queue_saturated",
                    McpToolDispatchStage::QueueAdmission.as_str(),
                    true,
                    format!("tool '{tool_name}' could not enter the bounded server request queue"),
                )
            })?;

        let deadline_micros =
            i64::try_from(policy.execution_duration().as_micros()).map_err(|_| {
                TraceDecayError::Config {
                    message: "MCP request deadline exceeds the domain clock".to_owned(),
                }
            })?;
        let deadline_wall =
            started
                .wall
                .0
                .checked_add(deadline_micros)
                .ok_or_else(|| TraceDecayError::Config {
                    message: "MCP request deadline exceeds the domain clock".to_owned(),
                })?;
        let deadline =
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(deadline_wall))
                .map_err(|error| TraceDecayError::Config {
                    message: format!("invalid MCP request deadline: {error}"),
                })?;
        let request_id =
            tracedecay_application::RequestId::new(request_key.to_owned()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("invalid MCP request identity: {error}"),
                }
            })?;
        let cancellation = tracedecay_application::CancellationSignal::active(format!(
            "cancellation.{request_key}"
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not create MCP cancellation signal: {error}"),
        })?;
        let termination = Arc::new(AtomicU8::new(McpRequestTermination::Active as u8));
        let registration;
        let pre_cancelled = {
            let mut requests = lock(&self.inner.requests);
            prune_pending_cancellations(&mut requests, tokio::time::Instant::now());
            if requests.active.contains_key(request_key) {
                return Err(TraceDecayError::mcp_tool_dispatch(
                    "tool_dispatch_duplicate_request_id",
                    McpToolDispatchStage::QueueAdmission.as_str(),
                    false,
                    format!(
                        "tool '{tool_name}' reused request id '{request_key}' while its prior request is active"
                    ),
                ));
            }
            registration = self.inner.next_registration.fetch_add(1, Ordering::AcqRel);
            let pre_cancelled = policy.externally_cancellable
                && requests.pending_cancellations.remove(request_key).is_some();
            if !policy.externally_cancellable {
                requests.pending_cancellations.remove(request_key);
            }
            requests.active.insert(
                request_key.to_owned(),
                ActiveMcpRequest {
                    registration,
                    cancellation: cancellation.clone(),
                    termination: Arc::clone(&termination),
                    externally_cancellable: policy.externally_cancellable,
                },
            );
            pre_cancelled
        };
        if pre_cancelled {
            terminate_signal(
                &termination,
                &cancellation,
                McpRequestTermination::Cancelled,
            );
        }

        Ok(McpToolDispatchControl {
            inner: Arc::new(McpToolDispatchControlInner {
                tool_name: Arc::from(tool_name),
                request_id,
                policy,
                deadline,
                deadline_at,
                response_deadline_at,
                cancellation,
                termination,
                _admission_permit: admission_permit,
                _lease: McpRequestLease {
                    registry: Arc::clone(&self.inner),
                    request_key: request_key.to_owned(),
                    registration,
                },
                worker_state: McpToolWorkerState::default(),
                reaper: self.inner.reaper.clone(),
            }),
        })
    }

    /// Cancels a live request or retains the notification until admission.
    ///
    /// Retention is bounded by count and age. At capacity the oldest orphaned
    /// notification is evicted so a current cancellation is never silently
    /// discarded merely because an earlier request never arrived.
    pub(crate) fn cancel_or_retain(&self, request_key: &str) -> bool {
        let now = tokio::time::Instant::now();
        let mut requests = lock(&self.inner.requests);
        if let Some(active) = requests.active.get(request_key) {
            return active.externally_cancellable
                && terminate_signal(
                    &active.termination,
                    &active.cancellation,
                    McpRequestTermination::Cancelled,
                );
        }
        prune_pending_cancellations(&mut requests, now);
        if requests.pending_cancellations.len() >= MAX_PENDING_CANCELLATIONS
            && let Some(oldest) = requests
                .pending_cancellations
                .iter()
                .min_by_key(|(_, retained_at)| **retained_at)
                .map(|(key, _)| key.clone())
        {
            requests.pending_cancellations.remove(&oldest);
        }
        requests
            .pending_cancellations
            .insert(request_key.to_owned(), now);
        true
    }

    pub(crate) fn cancel_all_live(&self) -> usize {
        lock(&self.inner.requests)
            .active
            .values()
            .filter(|request| {
                terminate_signal(
                    &request.termination,
                    &request.cancellation,
                    McpRequestTermination::Shutdown,
                )
            })
            .count()
    }

    pub(crate) fn shutdown_live(&self, request_key: &str) -> bool {
        let requests = lock(&self.inner.requests);
        requests.active.get(request_key).is_some_and(|request| {
            terminate_signal(
                &request.termination,
                &request.cancellation,
                McpRequestTermination::Shutdown,
            )
        })
    }

    pub(crate) async fn shutdown_workers(&self, timeout: Duration) -> McpWorkerReaperShutdown {
        self.inner.reaper.shutdown(timeout).await
    }

    #[cfg(test)]
    fn retained_cancellation_count(&self) -> usize {
        lock(&self.inner.requests).pending_cancellations.len()
    }
}

fn prune_pending_cancellations(requests: &mut McpRequestRegistryState, now: tokio::time::Instant) {
    requests.pending_cancellations.retain(|_, retained_at| {
        now.checked_duration_since(*retained_at)
            .is_none_or(|age| age <= PENDING_CANCELLATION_RETENTION)
    });
}

struct McpRequestLease {
    registry: Arc<McpRequestRegistryInner>,
    request_key: String,
    registration: u64,
}

impl Drop for McpRequestLease {
    fn drop(&mut self) {
        let mut requests = lock(&self.registry.requests);
        if requests
            .active
            .get(&self.request_key)
            .is_some_and(|active| active.registration == self.registration)
        {
            requests.active.remove(&self.request_key);
        }
    }
}

#[derive(Clone)]
pub(crate) struct McpToolDispatchControl {
    inner: Arc<McpToolDispatchControlInner>,
}

struct McpToolDispatchControlInner {
    tool_name: Arc<str>,
    request_id: tracedecay_application::RequestId,
    policy: McpToolLifecyclePolicy,
    deadline: tracedecay_application::Deadline,
    deadline_at: tokio::time::Instant,
    response_deadline_at: tokio::time::Instant,
    cancellation: tracedecay_application::CancellationSignal,
    termination: Arc<AtomicU8>,
    _admission_permit: OwnedSemaphorePermit,
    _lease: McpRequestLease,
    worker_state: McpToolWorkerState,
    reaper: McpWorkerSettlementReaper,
}

impl McpToolDispatchControl {
    pub(crate) fn request_id(&self) -> tracedecay_application::RequestId {
        self.inner.request_id.clone()
    }

    pub(crate) fn deadline(&self) -> tracedecay_application::Deadline {
        self.inner.deadline.clone()
    }

    pub(crate) fn cancellation(&self) -> tracedecay_application::CancellationSignal {
        self.inner.cancellation.clone()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancellation.is_cancelled()
    }

    pub(crate) fn externally_cancellable(&self) -> bool {
        self.inner.policy.externally_cancellable
    }

    pub(crate) fn cancel_from_transport(&self) -> bool {
        self.externally_cancellable() && self.terminate(McpRequestTermination::Cancelled)
    }

    pub(crate) fn check(&self, stage: McpToolDispatchStage) -> Result<()> {
        if stage != McpToolDispatchStage::ResponseWrite
            && (McpRequestTermination::from_raw(self.inner.termination.load(Ordering::Acquire))
                != McpRequestTermination::Active
                || self.is_cancelled())
        {
            return Err(self.terminal_error(stage));
        }
        if tokio::time::Instant::now() >= self.stage_deadline(stage) {
            self.terminate(McpRequestTermination::Deadline);
            return Err(if stage == McpToolDispatchStage::ResponseWrite {
                self.deadline_error(stage)
            } else {
                self.terminal_error(stage)
            });
        }
        Ok(())
    }

    pub(crate) async fn run<T, F>(&self, stage: McpToolDispatchStage, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        self.check(stage)?;
        let stage_deadline = self.stage_deadline(stage);
        tokio::pin!(future);
        if stage == McpToolDispatchStage::ResponseWrite {
            return tokio::select! {
                biased;
                () = tokio::time::sleep_until(stage_deadline) => {
                    self.terminate(McpRequestTermination::Deadline);
                    Err(self.deadline_error(stage))
                }
                result = &mut future => result,
            };
        }
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(stage_deadline) => {
                self.terminate(McpRequestTermination::Deadline);
                Err(self.terminal_error(stage))
            }
            () = crate::daemon_client::wait_for_cancellation(self.inner.cancellation.clone()) => {
                Err(self.terminal_error(stage))
            }
            result = &mut future => result,
        }
    }

    pub(crate) async fn run_value<T, F>(&self, stage: McpToolDispatchStage, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        self.run(stage, async move { Ok(future.await) }).await
    }

    pub(crate) async fn run_handler<T, F>(&self, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        self.run(McpToolDispatchStage::Handler, future).await
    }

    pub(crate) fn reserve_join_required_worker(
        &self,
        stage: McpToolDispatchStage,
    ) -> Result<McpToolWorkerReservation> {
        self.check(stage)?;
        self.inner.reaper.reserve(&self.inner.tool_name, stage)
    }

    pub(crate) async fn run_owned_join_required<T: Send + 'static>(
        &self,
        stage: McpToolDispatchStage,
        reservation: McpToolWorkerReservation,
        worker: tokio::task::JoinHandle<Result<T>>,
    ) -> Result<T> {
        let mut worker = OwnedMcpToolWorker::new(self, reservation, worker);
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(self.inner.deadline_at) => {
                self.terminate(McpRequestTermination::Deadline);
                Err(self.terminal_error(stage))
            }
            () = crate::daemon_client::wait_for_cancellation(self.inner.cancellation.clone()) => {
                self.wait_for_worker_cleanup(stage, &mut worker).await;
                Err(self.terminal_error(stage))
            }
            result = worker.wait(stage) => {
                worker.mark_joined();
                result
            }
        }
    }

    async fn wait_for_worker_cleanup<T: Send + 'static>(
        &self,
        stage: McpToolDispatchStage,
        worker: &mut OwnedMcpToolWorker<T>,
    ) {
        let cleanup_deadline = tokio::time::Instant::now()
            .checked_add(self.inner.policy.cancellation_cleanup)
            .map_or(self.inner.deadline_at, |candidate| {
                candidate.min(self.inner.deadline_at)
            });
        tokio::select! {
            () = tokio::time::sleep_until(cleanup_deadline) => {}
            _ = worker.wait(stage) => worker.mark_joined(),
        }
    }

    pub(crate) fn worker_receipt_snapshot(
        &self,
    ) -> (
        McpToolWorkerSettlement,
        Option<McpWorkerReconciliationReceipt>,
    ) {
        let (started, active, pending, reconciliation_id) = self.inner.worker_state.snapshot();
        if !started {
            return (McpToolWorkerSettlement::NotStarted, None);
        }
        if active == 0 && pending == 0 {
            return (McpToolWorkerSettlement::Joined, None);
        }
        let status = self.inner.reaper.reconciliation_status(reconciliation_id);
        let (_, active_after, pending_after, id_after) = self.inner.worker_state.snapshot();
        if active_after == 0 && pending_after == 0 {
            return (McpToolWorkerSettlement::Joined, None);
        }
        let status = if id_after == reconciliation_id {
            status
        } else {
            self.inner.reaper.reconciliation_status(id_after)
        };
        (
            McpToolWorkerSettlement::Indeterminate,
            Some(McpWorkerReconciliationReceipt {
                id: id_after,
                status: status.unwrap_or(McpWorkerReconciliationStatus::Unavailable),
            }),
        )
    }

    fn terminal_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        match McpRequestTermination::from_raw(self.inner.termination.load(Ordering::Acquire)) {
            McpRequestTermination::Deadline => self.deadline_error(stage),
            McpRequestTermination::Shutdown => self.shutdown_error(stage),
            McpRequestTermination::Active
                if tokio::time::Instant::now() >= self.inner.deadline_at =>
            {
                self.deadline_error(stage)
            }
            McpRequestTermination::Active | McpRequestTermination::Cancelled => {
                self.cancelled_error(stage)
            }
        }
    }

    fn stage_deadline(&self, stage: McpToolDispatchStage) -> tokio::time::Instant {
        if stage == McpToolDispatchStage::ResponseWrite {
            self.inner.response_deadline_at
        } else {
            self.inner.deadline_at
        }
    }

    fn terminate(&self, termination: McpRequestTermination) -> bool {
        terminate_signal(
            &self.inner.termination,
            &self.inner.cancellation,
            termination,
        )
    }

    fn deadline_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        dispatch_error(
            &self.inner.tool_name,
            self.inner.policy.maximum_duration,
            "tool_dispatch_deadline_exceeded",
            stage,
            true,
        )
    }

    fn cancelled_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        TraceDecayError::mcp_tool_dispatch(
            "tool_dispatch_cancelled",
            stage.as_str(),
            true,
            format!(
                "tool '{}' was cancelled during {}",
                self.inner.tool_name,
                stage.as_str()
            ),
        )
    }

    fn shutdown_error(&self, stage: McpToolDispatchStage) -> TraceDecayError {
        TraceDecayError::mcp_tool_dispatch(
            "tool_dispatch_shutdown",
            stage.as_str(),
            true,
            format!(
                "tool '{}' was cancelled during {} because the server is shutting down",
                self.inner.tool_name,
                stage.as_str()
            ),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum McpRequestTermination {
    Active = 0,
    Cancelled = 1,
    Shutdown = 2,
    Deadline = 3,
}

impl McpRequestTermination {
    const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Cancelled,
            2 => Self::Shutdown,
            3 => Self::Deadline,
            _ => Self::Active,
        }
    }
}

fn terminate_signal(
    termination: &AtomicU8,
    cancellation: &tracedecay_application::CancellationSignal,
    requested: McpRequestTermination,
) -> bool {
    if termination
        .compare_exchange(
            McpRequestTermination::Active as u8,
            requested as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    cancellation.cancel(tracedecay_application::clock::now_micros())
}

fn dispatch_error(
    tool_name: &str,
    maximum_duration: Duration,
    reason_code: &'static str,
    stage: McpToolDispatchStage,
    retryable: bool,
) -> TraceDecayError {
    TraceDecayError::mcp_tool_dispatch(
        reason_code,
        stage.as_str(),
        retryable,
        format!(
            "tool '{tool_name}' exceeded its {}ms absolute request deadline",
            maximum_duration.as_millis()
        ),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpToolWorkerSettlement {
    NotStarted,
    Joined,
    Indeterminate,
}

impl McpToolWorkerSettlement {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Joined => "joined",
            Self::Indeterminate => "indeterminate",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpWorkerReconciliationStatus {
    Pending,
    Joined,
    Failed,
    Unavailable,
}

impl McpWorkerReconciliationStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Joined => "joined",
            Self::Failed => "failed",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct McpWorkerReconciliationReceipt {
    pub(crate) id: u64,
    pub(crate) status: McpWorkerReconciliationStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpWorkerReaperState {
    Running,
    Draining,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpWorkerReaperShutdown {
    Complete { reconciliations: usize },
    Retryable { pending: usize },
}

#[derive(Clone)]
struct McpWorkerSettlementReaper {
    inner: Arc<McpWorkerSettlementReaperInner>,
}

struct McpWorkerSettlementReaperInner {
    state: AtomicU8,
    admission_gate: Mutex<()>,
    shutdown_gate: tokio::sync::Mutex<()>,
    permits: Arc<Semaphore>,
    next_id: AtomicU64,
    records: Mutex<BTreeMap<u64, McpWorkerSettlementRecord>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

struct McpWorkerSettlementRecord {
    status: McpWorkerReconciliationStatus,
}

impl McpWorkerSettlementReaper {
    fn new() -> Self {
        Self {
            inner: Arc::new(McpWorkerSettlementReaperInner {
                state: AtomicU8::new(0),
                admission_gate: Mutex::new(()),
                shutdown_gate: tokio::sync::Mutex::new(()),
                permits: Arc::new(Semaphore::new(MAX_PENDING_WORKER_SETTLEMENTS)),
                next_id: AtomicU64::new(1),
                records: Mutex::new(BTreeMap::new()),
                tasks: Mutex::new(Vec::new()),
            }),
        }
    }

    fn state(&self) -> McpWorkerReaperState {
        match self.inner.state.load(Ordering::Acquire) {
            0 => McpWorkerReaperState::Running,
            1 => McpWorkerReaperState::Draining,
            _ => McpWorkerReaperState::Stopped,
        }
    }

    fn reserve(
        &self,
        tool_name: &str,
        stage: McpToolDispatchStage,
    ) -> Result<McpToolWorkerReservation> {
        let _admission = lock(&self.inner.admission_gate);
        if self.state() != McpWorkerReaperState::Running {
            return Err(TraceDecayError::mcp_tool_dispatch(
                "tool_dispatch_reaper_unavailable",
                stage.as_str(),
                true,
                "server worker-settlement reaper is draining; retry the request",
            ));
        }
        self.reap_finished();
        let permit = Arc::clone(&self.inner.permits)
            .try_acquire_owned()
            .map_err(|_| {
                TraceDecayError::mcp_tool_dispatch(
                    "tool_dispatch_reaper_saturated",
                    stage.as_str(),
                    true,
                    format!("tool '{tool_name}' could not reserve worker settlement capacity"),
                )
            })?;
        let reconciliation_id = self.inner.next_id.fetch_add(1, Ordering::AcqRel);
        lock(&self.inner.records).insert(
            reconciliation_id,
            McpWorkerSettlementRecord {
                status: McpWorkerReconciliationStatus::Pending,
            },
        );
        Ok(McpToolWorkerReservation {
            reaper: self.clone(),
            reconciliation_id,
            permit: Some(permit),
        })
    }

    fn reconciliation_status(
        &self,
        reconciliation_id: u64,
    ) -> Option<McpWorkerReconciliationStatus> {
        self.reap_finished();
        lock(&self.inner.records)
            .get(&reconciliation_id)
            .map(|record| record.status)
    }

    async fn shutdown(&self, timeout: Duration) -> McpWorkerReaperShutdown {
        let _shutdown = self.inner.shutdown_gate.lock().await;
        if self.state() == McpWorkerReaperState::Stopped {
            return McpWorkerReaperShutdown::Complete { reconciliations: 0 };
        }
        {
            let _admission = lock(&self.inner.admission_gate);
            self.inner.state.store(1, Ordering::Release);
        }
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(tokio::time::Instant::now);
        let mut reconciliations = 0usize;

        loop {
            let task = lock(&self.inner.tasks).pop();
            let Some(mut task) = task else {
                if self.inner.permits.available_permits() == MAX_PENDING_WORKER_SETTLEMENTS {
                    self.inner.state.store(2, Ordering::Release);
                    return McpWorkerReaperShutdown::Complete { reconciliations };
                }
                if tokio::time::Instant::now() >= deadline {
                    return McpWorkerReaperShutdown::Retryable {
                        pending: MAX_PENDING_WORKER_SETTLEMENTS
                            .saturating_sub(self.inner.permits.available_permits()),
                    };
                }
                tokio::task::yield_now().await;
                continue;
            };

            tokio::select! {
                biased;
                result = &mut task => {
                    reconciliations = reconciliations.saturating_add(1);
                    if let Err(error) = result {
                        tracing::warn!(error = %error, "MCP worker settlement task failed during shutdown");
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    lock(&self.inner.tasks).push(task);
                    return McpWorkerReaperShutdown::Retryable {
                        pending: MAX_PENDING_WORKER_SETTLEMENTS
                            .saturating_sub(self.inner.permits.available_permits()),
                    };
                }
            }
        }
    }

    fn handoff<T: Send + 'static>(
        &self,
        reconciliation_id: u64,
        permit: OwnedSemaphorePermit,
        worker: tokio::task::JoinHandle<Result<T>>,
        worker_state: McpToolWorkerState,
    ) {
        worker_state.handoff(reconciliation_id);
        let settlement = PendingMcpWorkerSettlement {
            reaper: self.clone(),
            reconciliation_id,
            worker_state: Some(worker_state),
            permit: Some(permit),
            worker: Some(worker),
        };
        let task = tokio::spawn(settlement.settle());
        lock(&self.inner.tasks).push(task);
    }

    fn publish(
        &self,
        reconciliation_id: u64,
        status: McpWorkerReconciliationStatus,
        worker_state: McpToolWorkerState,
    ) {
        let mut records = lock(&self.inner.records);
        if let Some(record) = records.get_mut(&reconciliation_id) {
            record.status = status;
        }
        worker_state.reconciled();
        while records.len() > MAX_RETAINED_WORKER_SETTLEMENTS {
            let Some(oldest) = records
                .iter()
                .find(|(_, record)| record.status != McpWorkerReconciliationStatus::Pending)
                .map(|(id, _)| *id)
            else {
                break;
            };
            records.remove(&oldest);
        }
    }

    fn reap_finished(&self) {
        lock(&self.inner.tasks).retain(|task| !task.is_finished());
    }
}

struct PendingMcpWorkerSettlement<T: Send + 'static> {
    reaper: McpWorkerSettlementReaper,
    reconciliation_id: u64,
    worker_state: Option<McpToolWorkerState>,
    permit: Option<OwnedSemaphorePermit>,
    worker: Option<tokio::task::JoinHandle<Result<T>>>,
}

impl<T: Send + 'static> PendingMcpWorkerSettlement<T> {
    async fn settle(mut self) {
        let status = match self.worker.as_mut() {
            Some(worker) => match worker.await {
                Ok(_) => McpWorkerReconciliationStatus::Joined,
                Err(_) => McpWorkerReconciliationStatus::Failed,
            },
            None => McpWorkerReconciliationStatus::Failed,
        };
        self.worker.take();
        self.complete(status);
    }

    fn complete(mut self, status: McpWorkerReconciliationStatus) {
        if let Some(worker_state) = self.worker_state.take() {
            self.reaper
                .publish(self.reconciliation_id, status, worker_state);
        }
    }
}

impl<T: Send + 'static> Drop for PendingMcpWorkerSettlement<T> {
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else {
            if let Some(worker_state) = self.worker_state.take() {
                self.reaper.publish(
                    self.reconciliation_id,
                    McpWorkerReconciliationStatus::Failed,
                    worker_state,
                );
            }
            return;
        };
        worker.abort();
        let Some(worker_state) = self.worker_state.take() else {
            return;
        };
        let permit = self.permit.take();
        let reaper = self.reaper.clone();
        let task_reaper = reaper.clone();
        let reconciliation_id = self.reconciliation_id;
        let task = tokio::spawn(async move {
            let _ = worker.await;
            task_reaper.publish(
                reconciliation_id,
                McpWorkerReconciliationStatus::Failed,
                worker_state,
            );
            drop(permit);
        });
        lock(&reaper.inner.tasks).push(task);
    }
}

pub(crate) struct McpToolWorkerReservation {
    reaper: McpWorkerSettlementReaper,
    reconciliation_id: u64,
    permit: Option<OwnedSemaphorePermit>,
}

impl McpToolWorkerReservation {
    pub(crate) fn reconciliation_id(&self) -> u64 {
        self.reconciliation_id
    }

    fn joined(&mut self) {
        self.permit.take();
        lock(&self.reaper.inner.records).remove(&self.reconciliation_id);
    }

    fn handoff<T: Send + 'static>(
        mut self,
        worker: tokio::task::JoinHandle<Result<T>>,
        worker_state: McpToolWorkerState,
    ) {
        if let Some(permit) = self.permit.take() {
            self.reaper
                .handoff(self.reconciliation_id, permit, worker, worker_state);
        } else {
            worker.abort();
        }
    }
}

impl Drop for McpToolWorkerReservation {
    fn drop(&mut self) {
        if self.permit.is_some() {
            lock(&self.reaper.inner.records).remove(&self.reconciliation_id);
        }
    }
}

#[derive(Clone, Default)]
struct McpToolWorkerState {
    started: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
    latest_reconciliation_id: Arc<AtomicU64>,
}

impl McpToolWorkerState {
    fn start(&self, reconciliation_id: u64) {
        self.started.store(true, Ordering::Release);
        self.latest_reconciliation_id
            .store(reconciliation_id, Ordering::Release);
        self.active.fetch_add(1, Ordering::AcqRel);
    }

    fn joined(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }

    fn handoff(&self, reconciliation_id: u64) {
        self.pending.fetch_add(1, Ordering::AcqRel);
        self.latest_reconciliation_id
            .store(reconciliation_id, Ordering::Release);
        self.active.fetch_sub(1, Ordering::AcqRel);
    }

    fn reconciled(&self) {
        self.pending.fetch_sub(1, Ordering::AcqRel);
    }

    fn snapshot(&self) -> (bool, usize, usize, u64) {
        (
            self.started.load(Ordering::Acquire),
            self.active.load(Ordering::Acquire),
            self.pending.load(Ordering::Acquire),
            self.latest_reconciliation_id.load(Ordering::Acquire),
        )
    }
}

struct OwnedMcpToolWorker<T: Send + 'static> {
    control: McpToolDispatchControl,
    reservation: Option<McpToolWorkerReservation>,
    worker: Option<tokio::task::JoinHandle<Result<T>>>,
    joined: bool,
}

impl<T: Send + 'static> OwnedMcpToolWorker<T> {
    fn new(
        control: &McpToolDispatchControl,
        reservation: McpToolWorkerReservation,
        worker: tokio::task::JoinHandle<Result<T>>,
    ) -> Self {
        control
            .inner
            .worker_state
            .start(reservation.reconciliation_id());
        Self {
            control: control.clone(),
            reservation: Some(reservation),
            worker: Some(worker),
            joined: false,
        }
    }

    async fn wait(&mut self, stage: McpToolDispatchStage) -> Result<T> {
        let Some(worker) = self.worker.as_mut() else {
            return Err(TraceDecayError::mcp_tool_dispatch(
                "tool_dispatch_worker_missing",
                stage.as_str(),
                false,
                "join-required worker ownership was already consumed",
            ));
        };
        worker.await.map_err(|error| {
            TraceDecayError::mcp_tool_dispatch(
                "tool_dispatch_worker_failed",
                stage.as_str(),
                true,
                format!("join-required worker failed: {error}"),
            )
        })?
    }

    fn mark_joined(&mut self) {
        if self.joined {
            return;
        }
        self.joined = true;
        self.worker = None;
        self.control.inner.worker_state.joined();
        if let Some(mut reservation) = self.reservation.take() {
            reservation.joined();
        }
    }
}

impl<T: Send + 'static> Drop for OwnedMcpToolWorker<T> {
    fn drop(&mut self) {
        if !self.joined
            && let (Some(reservation), Some(worker)) = (self.reservation.take(), self.worker.take())
        {
            reservation.handoff(worker, self.control.inner.worker_state.clone());
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
