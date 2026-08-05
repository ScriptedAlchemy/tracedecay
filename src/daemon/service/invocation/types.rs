//! Shared retained-state shapes and small daemon-private types used across the invocation split.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AdvisoryHookOrchestrationAdmissionV1 {
    Enqueued,
    Warming,
    Backpressured,
    UnsupportedTrigger,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdvisoryHookOrchestrationTriggerV1 {
    SavedEdit,
    Stop,
    Explicit,
}

#[derive(Clone)]
pub(crate) struct AdvisoryHookOrchestrationRequestV1 {
    pub hook: AdmittedContextScoutHookV1,
    pub lifecycle: Option<ContextScoutLifecycleAddressV1>,
    pub hook_configuration_revision: u64,
    pub trigger: AdvisoryHookOrchestrationTriggerV1,
    pub(super) completion: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl AdvisoryHookOrchestrationRequestV1 {
    pub(in crate::daemon) fn from_envelope(
        envelope: HookEventEnvelopeV2,
        binding: &HookScopeBindingV1,
        lifecycle: Option<ContextScoutLifecycleAddressV1>,
        configuration_revision: u64,
        explicit: bool,
    ) -> Option<Self> {
        let hook = AdmittedContextScoutHookV1::new(envelope, binding)?;
        let trigger = if explicit {
            AdvisoryHookOrchestrationTriggerV1::Explicit
        } else {
            match &hook.envelope().event {
                HookEventV2::SavedEdit { .. } => AdvisoryHookOrchestrationTriggerV1::SavedEdit,
                HookEventV2::SessionBoundary {
                    boundary: HookBoundaryV1::End | HookBoundaryV1::TurnComplete,
                } => AdvisoryHookOrchestrationTriggerV1::Stop,
                _ => return None,
            }
        };
        Some(Self {
            hook,
            lifecycle,
            hook_configuration_revision: configuration_revision,
            trigger,
            completion: None,
        })
    }
}

/// Process-local bridge from an authenticated Hook V2 callback to the
/// project-open advisory owner. Implementations must return before provider,
/// retrieval, or model work begins.
pub(crate) trait AdvisoryHookOrchestrationPortV1: Send + Sync {
    fn admit(
        &self,
        request: AdvisoryHookOrchestrationRequestV1,
    ) -> AdvisoryHookOrchestrationAdmissionV1;
}

type AdvisoryHookOrchestrationFutureV1 = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type AdvisoryHookOrchestrationWorkV1 = dyn Fn(
        AdvisoryHookOrchestrationRequestV1,
        crate::application::context::CancellationToken,
    ) -> AdvisoryHookOrchestrationFutureV1
    + Send
    + Sync;
type AdvisoryHookOrchestrationEventKeyV1 = ([u8; 16], [u8; 16], [u8; 16]);
type AdvisoryHookOrchestrationAddressV1 = String;
type AdvisoryHookOrchestrationCompletionV1 = Arc<dyn Fn() + Send + Sync + 'static>;

struct AdvisoryHookOrchestrationInFlightEntryV1 {
    event: AdvisoryHookOrchestrationEventKeyV1,
    cancellation: crate::application::context::CancellationToken,
    superseded: AtomicBool,
    completions: StdMutex<Vec<AdvisoryHookOrchestrationCompletionV1>>,
}

#[derive(Default)]
struct AdvisoryHookOrchestrationInFlightV1 {
    addresses:
        BTreeMap<AdvisoryHookOrchestrationAddressV1, Arc<AdvisoryHookOrchestrationInFlightEntryV1>>,
    events: BTreeMap<
        AdvisoryHookOrchestrationEventKeyV1,
        Arc<AdvisoryHookOrchestrationInFlightEntryV1>,
    >,
}

pub(in crate::daemon::service) const MAX_COALESCED_ADVISORY_HOOK_COMPLETIONS: usize = 32;

struct AdvisoryHookTaskRegistryStateV1 {
    accepting: bool,
    tasks: tokio::task::JoinSet<()>,
}

impl Default for AdvisoryHookTaskRegistryStateV1 {
    fn default() -> Self {
        Self {
            accepting: true,
            tasks: tokio::task::JoinSet::new(),
        }
    }
}

#[derive(Default)]
struct AdvisoryHookTaskRegistryV1 {
    state: StdMutex<AdvisoryHookTaskRegistryStateV1>,
    cancellation: crate::application::context::CancellationToken,
}

impl AdvisoryHookTaskRegistryV1 {
    fn cancellation(&self) -> crate::application::context::CancellationToken {
        self.cancellation.clone()
    }

    fn spawn<F>(&self, task: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state.accepting || self.cancellation.is_cancelled() {
            return false;
        }
        while let Some(result) = state.tasks.try_join_next() {
            report_advisory_hook_task_result(result);
        }
        state.tasks.spawn_on(task, &handle);
        true
    }

    fn cancel(&self) {
        self.cancellation.cancel();
        if let Ok(mut state) = self.state.lock() {
            state.accepting = false;
        }
    }

    async fn cancel_and_join(&self) {
        self.cancellation.cancel();
        let mut tasks = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.accepting = false;
            std::mem::take(&mut state.tasks)
        };
        while let Some(result) = tasks.join_next().await {
            report_advisory_hook_task_result(result);
        }
    }
}

impl Drop for AdvisoryHookTaskRegistryV1 {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        state.tasks.abort_all();
    }
}

fn report_advisory_hook_task_result(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        tracing::error!(
            event = "advisory_hook_orchestration_worker_failed",
            error = %error,
            "daemon-owned advisory work did not terminate cleanly"
        );
    }
}

pub(crate) struct BoundedAdvisoryHookOrchestratorV1 {
    permits: Arc<Semaphore>,
    work: Arc<AdvisoryHookOrchestrationWorkV1>,
    in_flight: Arc<StdMutex<AdvisoryHookOrchestrationInFlightV1>>,
    tasks: Arc<AdvisoryHookTaskRegistryV1>,
}

impl BoundedAdvisoryHookOrchestratorV1 {
    fn new<F, Fut>(
        max_concurrent: usize,
        work: F,
        tasks: Arc<AdvisoryHookTaskRegistryV1>,
    ) -> Option<Arc<Self>>
    where
        F: Fn(
                AdvisoryHookOrchestrationRequestV1,
                crate::application::context::CancellationToken,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let work: Arc<AdvisoryHookOrchestrationWorkV1> =
            Arc::new(move |request, cancellation| Box::pin(work(request, cancellation)));
        (max_concurrent > 0).then(|| {
            Arc::new(Self {
                permits: Arc::new(Semaphore::new(max_concurrent)),
                work,
                in_flight: Arc::new(StdMutex::new(AdvisoryHookOrchestrationInFlightV1::default())),
                tasks,
            })
        })
    }

    fn stable_address(
        request: &AdvisoryHookOrchestrationRequestV1,
    ) -> Option<AdvisoryHookOrchestrationAddressV1> {
        let envelope = request.hook.envelope();
        canonical_sha256(&(
            "tracedecay.advisory-hook-address.v1",
            envelope.project_id,
            envelope.repository_id,
            envelope.worktree_id,
            envelope.protected_session_id,
            request.lifecycle.as_ref(),
        ))
        .ok()
        .map(|digest| digest.as_str().to_owned())
    }

    fn settle_operation(
        in_flight: &StdMutex<AdvisoryHookOrchestrationInFlightV1>,
        address: &AdvisoryHookOrchestrationAddressV1,
        operation: &Arc<AdvisoryHookOrchestrationInFlightEntryV1>,
        emit_terminal: bool,
    ) {
        let completions = {
            let mut in_flight = in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let completions = {
                let mut completions = operation
                    .completions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                std::mem::take(&mut *completions)
            };
            if in_flight
                .addresses
                .get(address)
                .is_some_and(|current| Arc::ptr_eq(current, operation))
            {
                in_flight.addresses.remove(address);
            }
            if in_flight
                .events
                .get(&operation.event)
                .is_some_and(|current| Arc::ptr_eq(current, operation))
            {
                in_flight.events.remove(&operation.event);
            }
            completions
        };
        if emit_terminal {
            for completion in completions {
                completion();
            }
        }
    }
}

impl AdvisoryHookOrchestrationPortV1 for BoundedAdvisoryHookOrchestratorV1 {
    fn admit(
        &self,
        mut request: AdvisoryHookOrchestrationRequestV1,
    ) -> AdvisoryHookOrchestrationAdmissionV1 {
        let cancellation = self.tasks.cancellation();
        if cancellation.is_cancelled() {
            return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
        }
        let envelope = request.hook.envelope();
        let event = (envelope.project_id, envelope.worktree_id, envelope.event_id);
        let Some(address) = Self::stable_address(&request) else {
            return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
        };
        let completion = request.completion.take();
        let (permit, operation) = {
            let Ok(mut in_flight) = self.in_flight.lock() else {
                return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
            };
            if let Some(incumbent) = in_flight.events.get(&event).cloned() {
                if let Some(completion) = completion {
                    let Ok(mut completions) = incumbent.completions.lock() else {
                        return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
                    };
                    if completions.len() >= MAX_COALESCED_ADVISORY_HOOK_COMPLETIONS {
                        return AdvisoryHookOrchestrationAdmissionV1::Backpressured;
                    }
                    completions.push(completion);
                }
                return AdvisoryHookOrchestrationAdmissionV1::Enqueued;
            }
            let permit = if let Some(incumbent) = in_flight.addresses.remove(&address) {
                incumbent
                    .superseded
                    .store(true, std::sync::atomic::Ordering::Release);
                incumbent.cancellation.cancel();
                None
            } else {
                let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
                    return AdvisoryHookOrchestrationAdmissionV1::Backpressured;
                };
                Some(permit)
            };
            let work_cancellation = crate::application::context::CancellationToken::new();
            let operation = Arc::new(AdvisoryHookOrchestrationInFlightEntryV1 {
                event,
                cancellation: work_cancellation,
                superseded: AtomicBool::new(false),
                completions: StdMutex::new(completion.into_iter().collect()),
            });
            in_flight
                .addresses
                .insert(address.clone(), Arc::clone(&operation));
            in_flight.events.insert(event, Arc::clone(&operation));
            (permit, operation)
        };
        let work = Arc::clone(&self.work);
        let in_flight = Arc::clone(&self.in_flight);
        let permits = Arc::clone(&self.permits);
        let (start, started) = tokio::sync::oneshot::channel();
        let task_address = address.clone();
        let task_operation = Arc::clone(&operation);
        let task = async move {
            let work_cancellation = task_operation.cancellation.clone();
            let admitted = tokio::select! {
                biased;
                () = work_cancellation.cancelled() => false,
                () = cancellation.cancelled() => false,
                result = started => result.is_ok(),
            };
            if !admitted {
                let superseded = task_operation
                    .superseded
                    .load(std::sync::atomic::Ordering::Acquire);
                Self::settle_operation(&in_flight, &task_address, &task_operation, superseded);
                return;
            }
            let permit = match permit {
                Some(permit) => Some(permit),
                None => tokio::select! {
                    biased;
                    () = work_cancellation.cancelled() => None,
                    () = cancellation.cancelled() => None,
                    permit = permits.acquire_owned() => permit.ok(),
                },
            };
            let Some(permit) = permit else {
                let superseded = task_operation
                    .superseded
                    .load(std::sync::atomic::Ordering::Acquire);
                Self::settle_operation(&in_flight, &task_address, &task_operation, superseded);
                return;
            };
            if cancellation.is_cancelled() || work_cancellation.is_cancelled() {
                let superseded = task_operation
                    .superseded
                    .load(std::sync::atomic::Ordering::Acquire);
                Self::settle_operation(&in_flight, &task_address, &task_operation, superseded);
                return;
            }
            let mut work_future = (work)(request, work_cancellation.clone());
            let emit_terminal = tokio::select! {
                biased;
                () = work_cancellation.cancelled() => {
                    (&mut work_future).await;
                    task_operation
                        .superseded
                        .load(std::sync::atomic::Ordering::Acquire)
                },
                () = cancellation.cancelled() => {
                    work_cancellation.cancel();
                    (&mut work_future).await;
                    task_operation
                        .superseded
                        .load(std::sync::atomic::Ordering::Acquire)
                },
                () = &mut work_future => true,
            };
            Self::settle_operation(&in_flight, &task_address, &task_operation, emit_terminal);
            drop(permit);
        };
        if !self.tasks.spawn(task) {
            Self::settle_operation(&self.in_flight, &address, &operation, false);
            return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
        }
        if start.send(()).is_err() {
            return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
        }
        AdvisoryHookOrchestrationAdmissionV1::Enqueued
    }
}

impl Drop for BoundedAdvisoryHookOrchestratorV1 {
    fn drop(&mut self) {
        self.tasks.cancel();
        if let Ok(in_flight) = self.in_flight.lock() {
            for entry in in_flight.events.values() {
                entry.cancellation.cancel();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AdvisoryRuntimeUnavailableReasonV1 {
    Cancelled,
    DeadlineExceeded,
    RegistrationFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum AdvisoryRuntimeReadinessV1 {
    Warming {
        started_at: UtcMicros,
    },
    Ready {
        started_at: UtcMicros,
        finished_at: UtcMicros,
    },
    Unavailable {
        started_at: UtcMicros,
        finished_at: UtcMicros,
        reason: AdvisoryRuntimeUnavailableReasonV1,
    },
}

enum DeferredAdvisoryHookOrchestratorStateV1 {
    Warming,
    Ready {
        runtime: Arc<dyn AdvisoryHookOrchestrationPortV1>,
        finished_at: UtcMicros,
    },
    Unavailable {
        reason: AdvisoryRuntimeUnavailableReasonV1,
        finished_at: UtcMicros,
    },
}

/// Retained post-open gateway for one project's advisory and Scout work.
///
/// The gateway is published before provider/model setup begins, so hook
/// admission distinguishes a live warming owner from a terminally unavailable
/// one. Setup has one claim and project-runtime retirement cancels that claim.
pub(crate) struct DeferredAdvisoryHookOrchestratorV1 {
    started_at: UtcMicros,
    state: StdMutex<DeferredAdvisoryHookOrchestratorStateV1>,
    setup_claimed: AtomicBool,
    setup_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    setup_settled: AtomicBool,
    setup_settled_notify: tokio::sync::Notify,
    cancellation: crate::application::context::CancellationToken,
    tasks: Arc<AdvisoryHookTaskRegistryV1>,
}

impl DeferredAdvisoryHookOrchestratorV1 {
    pub(crate) fn new(started_at: UtcMicros) -> Arc<Self> {
        Arc::new(Self {
            started_at,
            state: StdMutex::new(DeferredAdvisoryHookOrchestratorStateV1::Warming),
            setup_claimed: AtomicBool::new(false),
            setup_task: StdMutex::new(None),
            setup_settled: AtomicBool::new(false),
            setup_settled_notify: tokio::sync::Notify::new(),
            cancellation: crate::application::context::CancellationToken::new(),
            tasks: Arc::new(AdvisoryHookTaskRegistryV1::default()),
        })
    }

    pub(crate) fn new_bounded_runtime<F, Fut>(
        &self,
        max_concurrent: usize,
        work: F,
    ) -> Option<Arc<BoundedAdvisoryHookOrchestratorV1>>
    where
        F: Fn(
                AdvisoryHookOrchestrationRequestV1,
                crate::application::context::CancellationToken,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        BoundedAdvisoryHookOrchestratorV1::new(max_concurrent, work, Arc::clone(&self.tasks))
    }

    pub(crate) fn claim_setup(&self) -> bool {
        if self.cancellation.is_cancelled()
            || !matches!(
                self.state.lock().as_deref(),
                Ok(DeferredAdvisoryHookOrchestratorStateV1::Warming)
            )
        {
            return false;
        }
        self.setup_claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn cancellation(&self) -> crate::application::context::CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn retain_setup_task(&self, task: tokio::task::JoinHandle<()>) {
        *self
            .setup_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }

    pub(crate) fn mark_setup_settled(&self) {
        self.setup_settled
            .store(true, std::sync::atomic::Ordering::Release);
        self.setup_settled_notify.notify_waiters();
    }

    pub(crate) async fn join_setup(&self) {
        let task = {
            self.setup_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        };
        if let Some(task) = task {
            let _ = task.await;
        }
        while self
            .setup_claimed
            .load(std::sync::atomic::Ordering::Acquire)
            && !self
                .setup_settled
                .load(std::sync::atomic::Ordering::Acquire)
        {
            let notified = self.setup_settled_notify.notified();
            if self
                .setup_settled
                .load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
            notified.await;
        }
    }

    pub(crate) async fn cancel_and_join_setup(&self) {
        self.cancel();
        self.join_setup().await;
        self.tasks.cancel_and_join().await;
    }

    pub(crate) fn readiness(&self) -> AdvisoryRuntimeReadinessV1 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            DeferredAdvisoryHookOrchestratorStateV1::Warming => {
                AdvisoryRuntimeReadinessV1::Warming {
                    started_at: self.started_at,
                }
            }
            DeferredAdvisoryHookOrchestratorStateV1::Ready { finished_at, .. } => {
                AdvisoryRuntimeReadinessV1::Ready {
                    started_at: self.started_at,
                    finished_at: *finished_at,
                }
            }
            DeferredAdvisoryHookOrchestratorStateV1::Unavailable {
                reason,
                finished_at,
            } => AdvisoryRuntimeReadinessV1::Unavailable {
                started_at: self.started_at,
                finished_at: *finished_at,
                reason: *reason,
            },
        }
    }

    pub(crate) fn mark_ready(
        &self,
        runtime: Arc<dyn AdvisoryHookOrchestrationPortV1>,
        finished_at: UtcMicros,
    ) -> bool {
        if self.cancellation.is_cancelled() {
            self.mark_unavailable(AdvisoryRuntimeUnavailableReasonV1::Cancelled, finished_at);
            return false;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, DeferredAdvisoryHookOrchestratorStateV1::Warming) {
            return false;
        }
        *state = DeferredAdvisoryHookOrchestratorStateV1::Ready {
            runtime,
            finished_at,
        };
        true
    }

    pub(crate) fn mark_unavailable(
        &self,
        reason: AdvisoryRuntimeUnavailableReasonV1,
        finished_at: UtcMicros,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, DeferredAdvisoryHookOrchestratorStateV1::Warming) {
            return false;
        }
        *state = DeferredAdvisoryHookOrchestratorStateV1::Unavailable {
            reason,
            finished_at,
        };
        true
    }

    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
        self.tasks.cancel();
        let finished_at = now_micros();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            *state,
            DeferredAdvisoryHookOrchestratorStateV1::Warming
                | DeferredAdvisoryHookOrchestratorStateV1::Ready { .. }
        ) {
            *state = DeferredAdvisoryHookOrchestratorStateV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::Cancelled,
                finished_at,
            };
        }
    }
}

impl AdvisoryHookOrchestrationPortV1 for DeferredAdvisoryHookOrchestratorV1 {
    fn admit(
        &self,
        request: AdvisoryHookOrchestrationRequestV1,
    ) -> AdvisoryHookOrchestrationAdmissionV1 {
        match self.readiness() {
            AdvisoryRuntimeReadinessV1::Warming { .. } => {
                return AdvisoryHookOrchestrationAdmissionV1::Warming;
            }
            AdvisoryRuntimeReadinessV1::Unavailable { .. } => {
                return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
            }
            AdvisoryRuntimeReadinessV1::Ready { .. } => {}
        }
        let runtime = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*state {
                DeferredAdvisoryHookOrchestratorStateV1::Ready { runtime, .. } => {
                    Arc::clone(runtime)
                }
                DeferredAdvisoryHookOrchestratorStateV1::Warming
                | DeferredAdvisoryHookOrchestratorStateV1::Unavailable { .. } => {
                    return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
                }
            }
        };
        runtime.admit(request)
    }
}

pub(in crate::daemon::service) struct RegisteredAdvisoryHookOrchestrationRuntimeV1 {
    project_id: [u8; 16],
    worktree_id: [u8; 16],
    runtime: Arc<DeferredAdvisoryHookOrchestratorV1>,
}

impl RegisteredAdvisoryHookOrchestrationRuntimeV1 {
    pub(in crate::daemon::service) fn new(
        project_id: [u8; 16],
        worktree_id: [u8; 16],
        runtime: Arc<DeferredAdvisoryHookOrchestratorV1>,
    ) -> Self {
        Self {
            project_id,
            worktree_id,
            runtime,
        }
    }

    pub(in crate::daemon::service) fn matches(
        &self,
        project_id: [u8; 16],
        worktree_id: [u8; 16],
    ) -> bool {
        self.project_id == project_id && self.worktree_id == worktree_id
    }

    pub(in crate::daemon::service) fn runtime(&self) -> Arc<DeferredAdvisoryHookOrchestratorV1> {
        Arc::clone(&self.runtime)
    }
}

impl Drop for RegisteredAdvisoryHookOrchestrationRuntimeV1 {
    fn drop(&mut self) {
        self.runtime.cancel();
    }
}

type AdvisoryHookOrchestrationRegistryKey = ([u8; 16], [u8; 16]);
type AdvisoryHookOrchestrationRegistry = StdMutex<
    BTreeMap<AdvisoryHookOrchestrationRegistryKey, Weak<dyn AdvisoryHookOrchestrationPortV1>>,
>;

pub(super) fn advisory_hook_orchestration_registry() -> &'static AdvisoryHookOrchestrationRegistry {
    static REGISTRY: OnceLock<AdvisoryHookOrchestrationRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub(crate) fn admit_registered_advisory_hook_orchestration(
    envelope: HookEventEnvelopeV2,
    binding: HookScopeBindingV1,
    lifecycle: Option<ContextScoutLifecycleAddressV1>,
    configuration_revision: u64,
    explicit: bool,
    completion: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
) -> AdvisoryHookOrchestrationAdmissionV1 {
    let Some(mut request) = AdvisoryHookOrchestrationRequestV1::from_envelope(
        envelope,
        &binding,
        lifecycle,
        configuration_revision,
        explicit,
    ) else {
        return AdvisoryHookOrchestrationAdmissionV1::UnsupportedTrigger;
    };
    let Some(runtime) = advisory_hook_orchestration_registry()
        .lock()
        .ok()
        .and_then(|registry| {
            registry
                .get(&(
                    request.hook.envelope().project_id,
                    request.hook.envelope().worktree_id,
                ))
                .cloned()
        })
        .and_then(|runtime| runtime.upgrade())
    else {
        return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
    };
    request.completion = completion;
    runtime.admit(request)
}

pub(in crate::daemon::service) struct SwitchableFeedbackCycleRuntimeV1 {
    current: RwLock<Arc<dyn FeedbackCycleRuntimePort>>,
}

pub(in crate::daemon) fn observe_accepted_feedback_cycle_terminal(
    observations: &Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
    project_id: &ProjectId,
    request: &FeedbackCycleRequest,
    outcome: Plan26FeedbackOutcomeV1,
) {
    let trigger = match request.trigger {
        DiagnosticTrigger::DocumentSave => "document_save",
        DiagnosticTrigger::ExplicitDocumentDiagnostics => "explicit_document_diagnostics",
    };
    let Ok(subject) = canonical_sha256(&(
        "tracedecay.feedback.accepted-cycle.v1",
        project_id,
        &request.root_uri,
        &request.document_uri,
        trigger,
    )) else {
        return;
    };
    observations.observe_source_event_for_subject(
        subject,
        now_micros(),
        Plan26FeedbackSourceEventV1::Delivery {
            operation: Plan26FeedbackOperationV1::FeedbackCycle,
            route: Plan26DeliveryRouteV1::Lsp,
            outcome,
            item_count: 0,
            duration_micros: None,
        },
    );
}

pub(in crate::daemon::service) struct UnavailableFeedbackCycleRuntimeV1 {
    project_id: ProjectId,
    observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

impl UnavailableFeedbackCycleRuntimeV1 {
    pub(in crate::daemon::service) fn new(
        project_id: ProjectId,
        observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
    ) -> Self {
        Self {
            project_id,
            observations,
        }
    }
}

impl FeedbackCycleRuntimePort for UnavailableFeedbackCycleRuntimeV1 {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let project_id = self.project_id.clone();
        let observations = Arc::clone(&self.observations);
        Box::pin(async move {
            observe_accepted_feedback_cycle_terminal(
                &observations,
                &project_id,
                &request,
                Plan26FeedbackOutcomeV1::Unavailable,
            );
            Err(LspRuntimeFailure::new("feedback-cycle-unavailable"))
        })
    }
}

impl SwitchableFeedbackCycleRuntimeV1 {
    pub(in crate::daemon::service) fn new(current: Arc<dyn FeedbackCycleRuntimePort>) -> Self {
        Self {
            current: RwLock::new(current),
        }
    }

    pub(in crate::daemon::service) fn replace(
        &self,
        current: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Result<(), LspRuntimeFailure> {
        self.swap(current).map(|_| ())
    }

    pub(in crate::daemon::service) fn swap(
        &self,
        current: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Result<Arc<dyn FeedbackCycleRuntimePort>, LspRuntimeFailure> {
        let mut incumbent = self
            .current
            .write()
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-router"))?;
        Ok(std::mem::replace(&mut *incumbent, current))
    }

    pub(in crate::daemon::service) fn replace_if_same(
        &self,
        expected: &Arc<dyn FeedbackCycleRuntimePort>,
        replacement: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Result<bool, LspRuntimeFailure> {
        let mut current = self
            .current
            .write()
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-router"))?;
        if !Arc::ptr_eq(&current, expected) {
            return Ok(false);
        }
        *current = replacement;
        Ok(true)
    }
}

impl FeedbackCycleRuntimePort for SwitchableFeedbackCycleRuntimeV1 {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let current = self
            .current
            .read()
            .map(|current| Arc::clone(&current))
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-router"));
        Box::pin(async move { current?.execute(request).await })
    }
}

/// Retained daemon state for the typed LSP invocation operations.
#[derive(Clone)]
pub(in crate::daemon::service) struct RegisteredWorkRuntime {
    pub(super) database: Arc<crate::global_db::RegisteredGlobalDb>,
    pub(super) runtime:
        Arc<DaemonWorkRuntimeV1<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>>,
    pub(super) actor: ActorId,
    pub(super) grant: CapabilityGrantSnapshot,
    pub(super) authority_digest: ManifestDigest,
    pub(super) policy_digest: ManifestDigest,
    pub(super) configuration_digest: ManifestDigest,
}

impl RegisteredWorkRuntime {
    /// Takes the provider runtime out for shutdown, dropping the rest of the
    /// registration with it.
    pub(in crate::daemon::service) fn into_runtime(
        self,
    ) -> Arc<DaemonWorkRuntimeV1<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>> {
        self.runtime
    }
}

pub(in crate::daemon::service) struct RegisteredFeedbackRuntime {
    pub(super) project_id: ProjectId,
    pub(super) runtime: Arc<Pr12FeedbackRuntime>,
}

impl RegisteredFeedbackRuntime {
    pub(in crate::daemon::service) fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub(in crate::daemon::service) fn runtime(&self) -> Arc<Pr12FeedbackRuntime> {
        Arc::clone(&self.runtime)
    }

    pub(in crate::daemon::service) fn invocation_owner(&self) -> DaemonFeedbackInvocationOwner {
        DaemonFeedbackInvocationOwner::new(self.project_id.clone(), self.runtime.owner())
    }

    pub(in crate::daemon::service) fn source_observation_port(
        &self,
    ) -> Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync> {
        self.runtime.source_observation_port()
    }
}

#[derive(Clone)]
pub(in crate::daemon::service) struct RegisteredCallableCodeRuntime {
    pub(super) scope: ResolvedScope,
    pub(super) authorization: DaemonCallableCodeAuthorizationSource,
}

#[derive(Clone)]
pub(in crate::daemon::service) struct RegisteredConfigurationRuntime {
    pub(super) runtime: Arc<ProjectConfigurationRuntime>,
    pub(super) scope: ResolvedScope,
    pub(super) actor: ActorId,
    pub(super) grants: DaemonConfigurationGrantAuthority,
    pub(super) semantic_operation: Arc<OnceLock<Arc<ProductionSemanticConfigurationOperationV1>>>,
}

pub(super) struct RuntimeLspSession {
    pub(super) expires_at_ms: u64,
    pub(super) actor: RuntimeLspActor,
}

impl Drop for RuntimeLspSession {
    fn drop(&mut self) {
        // Every removal path (explicit detach, transport loss, TTL expiry, and
        // daemon shutdown) must cancel provider work and release overlays,
        // subscriptions, publications, and queued frames before the actor is
        // discarded.
        self.actor.expire();
    }
}

struct LspLeaseTask {
    generation: u64,
    cancellation: crate::application::context::CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl LspLeaseTask {
    async fn stop(self) -> Result<(), DaemonInvocationProblem> {
        self.cancellation.cancel();
        self.handle
            .await
            .map_err(|_| DaemonInvocationProblem::Unavailable)
    }

    fn abort(&self) {
        self.cancellation.cancel();
        self.handle.abort();
    }
}

struct LspLeaseTaskRegistryState {
    accepting: bool,
    next_generation: u64,
    tasks: BTreeMap<LspSessionId, LspLeaseTask>,
}

impl Default for LspLeaseTaskRegistryState {
    fn default() -> Self {
        Self {
            accepting: true,
            next_generation: 0,
            tasks: BTreeMap::new(),
        }
    }
}

/// Owns one bounded expiry task per disconnected session.
///
/// Each task waits behind a start gate until its generation and handle are
/// registered. This makes immediate completion observable by the owner rather
/// than leaving a completed handle behind.
///
/// Generations prevent an older task from retiring its replacement, and each
/// task holds only a weak registry reference so dropping the daemon aborts all
/// remaining work without creating an ownership cycle.
#[derive(Default)]
pub(super) struct LspLeaseTaskRegistry {
    state: StdMutex<LspLeaseTaskRegistryState>,
}

impl LspLeaseTaskRegistry {
    pub(super) async fn start<F>(
        self: &Arc<Self>,
        session_id: LspSessionId,
        task: F,
    ) -> Result<(), DaemonInvocationProblem>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let current_session_id = session_id.clone();
        let (previous, start, generation) = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            if !state.accepting {
                return Err(DaemonInvocationProblem::Unavailable);
            }
            let Some(generation) = state.next_generation.checked_add(1) else {
                return Err(DaemonInvocationProblem::Unavailable);
            };
            state.next_generation = generation;
            let cancellation = crate::application::context::CancellationToken::new();
            let task_cancellation = cancellation.clone();
            let task_registry = Arc::downgrade(self);
            let task_session_id = session_id.clone();
            let (start, started) = tokio::sync::oneshot::channel();
            let handle = tokio::spawn(async move {
                let admitted = tokio::select! {
                    result = started => result.is_ok(),
                    () = task_cancellation.cancelled() => false,
                };
                if admitted {
                    tokio::select! {
                        () = task => {}
                        () = task_cancellation.cancelled() => {}
                    }
                }
                if let Some(task_registry) = task_registry.upgrade() {
                    task_registry.finish(&task_session_id, generation);
                }
            });
            let previous = state.tasks.insert(
                session_id,
                LspLeaseTask {
                    generation,
                    cancellation,
                    handle,
                },
            );
            (previous, start, generation)
        };
        if let Some(previous) = previous {
            if previous.stop().await.is_err() {
                self.stop_generation(&current_session_id, Some(generation))
                    .await?;
                return Err(DaemonInvocationProblem::Unavailable);
            }
        }
        if start.send(()).is_err() {
            self.stop_generation(&current_session_id, Some(generation))
                .await?;
            return Err(DaemonInvocationProblem::Unavailable);
        }
        Ok(())
    }

    pub(super) async fn cancel(
        &self,
        session_id: &LspSessionId,
    ) -> Result<(), DaemonInvocationProblem> {
        self.stop_generation(session_id, None).await
    }

    pub(super) fn finish(&self, session_id: &LspSessionId, generation: u64) {
        self.take_generation(session_id, Some(generation));
    }

    async fn stop_generation(
        &self,
        session_id: &LspSessionId,
        generation: Option<u64>,
    ) -> Result<(), DaemonInvocationProblem> {
        if let Some(task) = self.take_generation(session_id, generation) {
            task.stop().await?;
        }
        Ok(())
    }

    fn take_generation(
        &self,
        session_id: &LspSessionId,
        generation: Option<u64>,
    ) -> Option<LspLeaseTask> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let matches = generation.is_none_or(|generation| {
            state
                .tasks
                .get(session_id)
                .is_some_and(|task| task.generation == generation)
        });
        matches.then(|| state.tasks.remove(session_id)).flatten()
    }

    pub(super) async fn shutdown(&self) -> Result<(), DaemonInvocationProblem> {
        let tasks = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.accepting = false;
            std::mem::take(&mut state.tasks)
        };
        let mut outcome = Ok(());
        for task in tasks.into_values() {
            if let Err(problem) = task.stop().await {
                outcome = Err(problem);
            }
        }
        outcome
    }

    #[cfg(test)]
    pub(super) fn active_tasks(&self) -> usize {
        match self.state.lock() {
            Ok(state) => state.tasks.len(),
            Err(poisoned) => poisoned.into_inner().tasks.len(),
        }
    }
}

impl Drop for LspLeaseTaskRegistry {
    fn drop(&mut self) {
        let state = match self.state.get_mut() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.accepting = false;
        for task in state.tasks.values() {
            task.abort();
        }
    }
}

pub(super) type RuntimeLspActor = DaemonLspRuntimeSession;

#[derive(Clone)]
pub(crate) struct DaemonLspInvocationOwner {
    pub(super) factory: Arc<DaemonLspSessionFactory>,
    pub(super) scope_grant: Option<CapabilityGrantSnapshot>,
    pub(super) scope_set_storage:
        Option<tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage>,
}

#[derive(Clone)]
pub(super) struct AuthorizedDaemonLspWorkspace {
    pub(super) scope_set: AuthorizedScopeSet,
    pub(super) factories: Vec<(AdmittedRoot, Arc<DaemonLspSessionFactory>)>,
}

impl DaemonLspInvocationOwner {
    pub(in crate::daemon::service) fn factory(&self) -> &Arc<DaemonLspSessionFactory> {
        &self.factory
    }

    pub(crate) fn new(factory: Arc<DaemonLspSessionFactory>) -> Self {
        Self {
            factory,
            scope_grant: None,
            scope_set_storage: None,
        }
    }

    pub(crate) fn authorized(
        factory: Arc<DaemonLspSessionFactory>,
        scope_grant: CapabilityGrantSnapshot,
        scope_set_storage: tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage,
    ) -> Self {
        Self {
            factory,
            scope_grant: Some(scope_grant),
            scope_set_storage: Some(scope_set_storage),
        }
    }
}

/// Admission binds a session to the workspace independently resolved by the
/// daemon before this protocol is invoked. Client root hints are never
/// authority.
#[derive(Clone, Debug)]
pub(super) struct AdmittedWorkspaceSessionAdmission {
    pub(super) workspace: AuthorizedLspWorkspace,
}

impl LspSessionAdmissionPort for AdmittedWorkspaceSessionAdmission {
    fn admit_lsp_session(
        &self,
        _request: &LspSessionOpenRequest,
        now_ms: u64,
    ) -> Result<AuthorizedLspSession, LspEndpointError> {
        let mut session_bytes = [0_u8; 16];
        let mut credential_bytes = [0_u8; 32];
        getrandom::getrandom(&mut session_bytes)
            .map_err(|_| LspEndpointError::AdmissionRejected)?;
        getrandom::getrandom(&mut credential_bytes)
            .map_err(|_| LspEndpointError::AdmissionRejected)?;
        let session_id = LspSessionId::new(format!("lsp-{}", hex::encode(session_bytes)))?;
        let credential = LspSessionCredential::new(credential_bytes.to_vec())?;
        Ok(AuthorizedLspSession {
            session_id,
            credential,
            workspace: self.workspace.clone(),
            expires_at_ms: now_ms.saturating_add(LSP_SESSION_TTL_MS),
        })
    }
}

#[derive(Clone)]
pub(super) struct SharedGitTransactionPort {
    pub(super) service: Arc<DaemonProjectGitIndexTransactionService>,
    pub(super) cancellation: Option<OperationEmitter>,
}

impl GitIndexTransactionPort for SharedGitTransactionPort {
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError> {
        self.service.preview(request)
    }

    fn apply(
        &self,
        request: &GitIndexApplyRequestV1,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        self.cancellation.as_ref().map_or_else(
            || self.service.apply(request),
            |emitter| {
                self.service
                    .apply_cancellable(request, || emitter.cancellation_requested_at())
            },
        )
    }

    fn recover(
        &self,
        request: &GitIndexRecoveryRequestV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionPortError> {
        self.service.recover(request)
    }
}
