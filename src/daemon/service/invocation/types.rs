//! Shared retained-state shapes and small daemon-private types used across the invocation split.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookOrchestrationAdmissionV1 {
    Enqueued,
    /// The retained post-open gateway is published but its bounded setup has
    /// not reached a terminal yet; the caller may retry after warmup.
    Warming,
    Backpressured,
    UnsupportedTrigger,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HookOrchestrationTriggerV1 {
    SavedEdit,
    Stop,
    Explicit,
}

#[derive(Clone)]
pub(crate) struct HookOrchestrationRequestV1 {
    pub hook: AdmittedContextScoutHookV1,
    pub lifecycle: Option<ContextScoutLifecycleAddressV1>,
    pub hook_configuration_revision: u64,
    pub trigger: HookOrchestrationTriggerV1,
    pub(super) completion: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl HookOrchestrationRequestV1 {
    pub(in crate::daemon) fn from_envelope(
        envelope: HookEventEnvelopeV2,
        binding: &HookScopeBindingV1,
        lifecycle: Option<ContextScoutLifecycleAddressV1>,
        configuration_revision: u64,
        explicit: bool,
    ) -> Option<Self> {
        let hook = AdmittedContextScoutHookV1::new(envelope, binding)?;
        let trigger = if explicit {
            HookOrchestrationTriggerV1::Explicit
        } else {
            match &hook.envelope().event {
                HookEventV2::SavedEdit { .. } => HookOrchestrationTriggerV1::SavedEdit,
                HookEventV2::SessionBoundary {
                    boundary: HookBoundaryV1::End | HookBoundaryV1::TurnComplete,
                } => HookOrchestrationTriggerV1::Stop,
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
pub(crate) trait HookOrchestrationPortV1: Send + Sync {
    fn admit(&self, request: HookOrchestrationRequestV1) -> HookOrchestrationAdmissionV1;
}

type HookOrchestrationFutureV1 = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type HookOrchestrationWorkV1 = dyn Fn(
        HookOrchestrationRequestV1,
        crate::application::context::CancellationToken,
    ) -> HookOrchestrationFutureV1
    + Send
    + Sync;
/// Exact hook identity: one project, one worktree, one hook event. Two
/// admissions that agree on all three describe the same boundary, so they must
/// share one cycle rather than start a second.
type HookOrchestrationEventKeyV1 = ([u8; 16], [u8; 16], [u8; 16]);
/// Stable per-session work address. A newer boundary at the same address
/// supersedes the running one instead of queueing behind it.
type HookOrchestrationAddressV1 = String;
type HookOrchestrationCompletionV1 = Arc<dyn Fn() + Send + Sync + 'static>;

struct HookOrchestrationInFlightEntryV1 {
    event: HookOrchestrationEventKeyV1,
    cancellation: crate::application::context::CancellationToken,
    superseded: std::sync::atomic::AtomicBool,
    completions: StdMutex<Vec<HookOrchestrationCompletionV1>>,
}

#[derive(Default)]
struct HookOrchestrationInFlightV1 {
    addresses: BTreeMap<HookOrchestrationAddressV1, Arc<HookOrchestrationInFlightEntryV1>>,
    events: BTreeMap<HookOrchestrationEventKeyV1, Arc<HookOrchestrationInFlightEntryV1>>,
}

/// Upper bound on admissions that may join one in-flight cycle. Beyond it the
/// caller is backpressured instead of queued, so a hook storm can never grow
/// unbounded retained state behind a single bounded operation.
pub(in crate::daemon::service) const MAX_COALESCED_HOOK_COMPLETIONS: usize = 32;

pub(crate) struct BoundedHookOrchestratorV1 {
    permits: Arc<Semaphore>,
    work: Arc<HookOrchestrationWorkV1>,
    in_flight: Arc<StdMutex<HookOrchestrationInFlightV1>>,
    cancellation: crate::application::context::CancellationToken,
}

impl BoundedHookOrchestratorV1 {
    pub(crate) fn new<F, Fut>(max_concurrent: usize, work: F) -> Option<Arc<Self>>
    where
        F: Fn(HookOrchestrationRequestV1, crate::application::context::CancellationToken) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let work: Arc<HookOrchestrationWorkV1> =
            Arc::new(move |request, cancellation| Box::pin(work(request, cancellation)));
        (max_concurrent > 0).then(|| {
            Arc::new(Self {
                permits: Arc::new(Semaphore::new(max_concurrent)),
                work,
                in_flight: Arc::new(StdMutex::new(HookOrchestrationInFlightV1::default())),
                cancellation: crate::application::context::CancellationToken::new(),
            })
        })
    }

    fn stable_address(request: &HookOrchestrationRequestV1) -> Option<HookOrchestrationAddressV1> {
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
        in_flight: &StdMutex<HookOrchestrationInFlightV1>,
        address: &HookOrchestrationAddressV1,
        operation: &Arc<HookOrchestrationInFlightEntryV1>,
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

impl HookOrchestrationPortV1 for BoundedHookOrchestratorV1 {
    fn admit(&self, mut request: HookOrchestrationRequestV1) -> HookOrchestrationAdmissionV1 {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return HookOrchestrationAdmissionV1::Unavailable;
        };
        let envelope = request.hook.envelope();
        let event = (envelope.project_id, envelope.worktree_id, envelope.event_id);
        let Some(address) = Self::stable_address(&request) else {
            return HookOrchestrationAdmissionV1::Unavailable;
        };
        let completion = request.completion.take();
        let (permit, operation) = {
            let Ok(mut in_flight) = self.in_flight.lock() else {
                return HookOrchestrationAdmissionV1::Unavailable;
            };
            if let Some(incumbent) = in_flight.events.get(&event).cloned() {
                // The exact boundary is already running. Join it: one cycle
                // terminates once and every joined admission observes that one
                // terminal, so a duplicate never consumes a second permit.
                if let Some(completion) = completion {
                    let Ok(mut completions) = incumbent.completions.lock() else {
                        return HookOrchestrationAdmissionV1::Unavailable;
                    };
                    if completions.len() >= MAX_COALESCED_HOOK_COMPLETIONS {
                        return HookOrchestrationAdmissionV1::Backpressured;
                    }
                    completions.push(completion);
                }
                return HookOrchestrationAdmissionV1::Enqueued;
            }
            let permit = if let Some(incumbent) = in_flight.addresses.remove(&address) {
                // A newer boundary at the same stable address supersedes the
                // incumbent: cancel it and inherit its permit once it settles.
                incumbent
                    .superseded
                    .store(true, std::sync::atomic::Ordering::Release);
                incumbent.cancellation.cancel();
                None
            } else {
                let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
                    return HookOrchestrationAdmissionV1::Backpressured;
                };
                Some(permit)
            };
            let work_cancellation = crate::application::context::CancellationToken::new();
            let operation = Arc::new(HookOrchestrationInFlightEntryV1 {
                event,
                cancellation: work_cancellation,
                superseded: std::sync::atomic::AtomicBool::new(false),
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
        let cancellation = self.cancellation.clone();
        let permits = Arc::clone(&self.permits);
        handle.spawn(async move {
            let work_cancellation = operation.cancellation.clone();
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
                let superseded = operation
                    .superseded
                    .load(std::sync::atomic::Ordering::Acquire);
                Self::settle_operation(&in_flight, &address, &operation, superseded);
                return;
            };
            if cancellation.is_cancelled() || work_cancellation.is_cancelled() {
                let superseded = operation
                    .superseded
                    .load(std::sync::atomic::Ordering::Acquire);
                Self::settle_operation(&in_flight, &address, &operation, superseded);
                return;
            }
            let mut work_future = (work)(request, work_cancellation.clone());
            // Only completed or superseded work emits terminals. Owner-level
            // cancellation reports nothing: silence is a normal result, and an
            // adapter must never invent a termination reason.
            let emit_terminal = tokio::select! {
                biased;
                () = work_cancellation.cancelled() => {
                    (&mut work_future).await;
                    operation
                        .superseded
                        .load(std::sync::atomic::Ordering::Acquire)
                },
                () = cancellation.cancelled() => {
                    work_cancellation.cancel();
                    (&mut work_future).await;
                    operation
                        .superseded
                        .load(std::sync::atomic::Ordering::Acquire)
                },
                () = &mut work_future => true,
            };
            Self::settle_operation(&in_flight, &address, &operation, emit_terminal);
            drop(permit);
        });
        HookOrchestrationAdmissionV1::Enqueued
    }
}

impl Drop for BoundedHookOrchestratorV1 {
    fn drop(&mut self) {
        // Retiring the owner cancels every worker it still holds. The workers
        // drop their in-flight entry without firing completions.
        self.cancellation.cancel();
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

enum DeferredHookOrchestratorStateV1 {
    Warming {
        started_at: UtcMicros,
        claimed: bool,
    },
    Ready {
        started_at: UtcMicros,
        runtime: Arc<dyn HookOrchestrationPortV1>,
        finished_at: UtcMicros,
    },
    Unavailable {
        started_at: UtcMicros,
        reason: AdvisoryRuntimeUnavailableReasonV1,
        finished_at: UtcMicros,
    },
}

/// Retained post-open gateway for one project's advisory and Scout work.
///
/// The gateway is published before provider/model setup begins, so hook
/// admission distinguishes a live warming owner from a terminally unavailable
/// one. Setup has one claim and project-runtime retirement cancels that claim.
pub(crate) struct DeferredHookOrchestratorV1 {
    state: StdMutex<DeferredHookOrchestratorStateV1>,
    setup_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancellation: crate::application::context::CancellationToken,
}

impl DeferredHookOrchestratorV1 {
    pub(crate) fn new(started_at: UtcMicros) -> Arc<Self> {
        Arc::new(Self {
            state: StdMutex::new(DeferredHookOrchestratorStateV1::Warming {
                started_at,
                claimed: false,
            }),
            setup_task: Mutex::new(None),
            cancellation: crate::application::context::CancellationToken::new(),
        })
    }

    pub(crate) fn claim_setup(&self) -> bool {
        if self.cancellation.is_cancelled() {
            return false;
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        match &mut *state {
            DeferredHookOrchestratorStateV1::Warming { claimed, .. } if !*claimed => {
                *claimed = true;
                true
            }
            // A deadline or registration terminal may be retried by a new
            // claim; an explicit cancellation is final.
            DeferredHookOrchestratorStateV1::Unavailable {
                reason:
                    AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded
                    | AdvisoryRuntimeUnavailableReasonV1::RegistrationFailed,
                ..
            } => {
                *state = DeferredHookOrchestratorStateV1::Warming {
                    started_at: now_micros(),
                    claimed: true,
                };
                true
            }
            DeferredHookOrchestratorStateV1::Warming { .. }
            | DeferredHookOrchestratorStateV1::Ready { .. }
            | DeferredHookOrchestratorStateV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::Cancelled,
                ..
            } => false,
        }
    }

    pub(crate) fn cancellation(&self) -> crate::application::context::CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn readiness(&self) -> AdvisoryRuntimeReadinessV1 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            DeferredHookOrchestratorStateV1::Warming { started_at, .. } => {
                AdvisoryRuntimeReadinessV1::Warming {
                    started_at: *started_at,
                }
            }
            DeferredHookOrchestratorStateV1::Ready {
                started_at,
                finished_at,
                ..
            } => AdvisoryRuntimeReadinessV1::Ready {
                started_at: *started_at,
                finished_at: *finished_at,
            },
            DeferredHookOrchestratorStateV1::Unavailable {
                started_at,
                reason,
                finished_at,
            } => AdvisoryRuntimeReadinessV1::Unavailable {
                started_at: *started_at,
                finished_at: *finished_at,
                reason: *reason,
            },
        }
    }

    pub(crate) fn mark_ready(
        &self,
        runtime: Arc<dyn HookOrchestrationPortV1>,
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
        let DeferredHookOrchestratorStateV1::Warming {
            started_at,
            claimed: true,
        } = *state
        else {
            return false;
        };
        *state = DeferredHookOrchestratorStateV1::Ready {
            started_at,
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
        let DeferredHookOrchestratorStateV1::Warming {
            started_at,
            claimed: true,
        } = *state
        else {
            return false;
        };
        *state = DeferredHookOrchestratorStateV1::Unavailable {
            started_at,
            reason,
            finished_at,
        };
        true
    }

    /// Retains the owning setup task, waiting for a finished predecessor to
    /// settle first. Fails after cancellation so retirement never races a new
    /// setup into a cancelled gateway.
    pub(crate) async fn retain_setup_task(
        &self,
        task: tokio::task::JoinHandle<()>,
    ) -> Result<(), tokio::task::JoinHandle<()>> {
        let task = task;
        loop {
            let incumbent = {
                let mut setup_task = self.setup_task.lock().await;
                if self.cancellation.is_cancelled() {
                    return Err(task);
                }
                match setup_task.take() {
                    Some(incumbent) => Some(incumbent),
                    None => {
                        *setup_task = Some(task);
                        return Ok(());
                    }
                }
            };
            if let Some(incumbent) = incumbent {
                let _ = incumbent.await;
            }
        }
    }

    pub(crate) async fn setup_task_finished(&self) {
        self.setup_task.lock().await.take();
    }

    pub(crate) async fn cancel_and_join(&self) {
        self.cancellation.cancel();
        let finished_at = now_micros();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let started_at = match &*state {
                DeferredHookOrchestratorStateV1::Warming { started_at, .. }
                | DeferredHookOrchestratorStateV1::Ready { started_at, .. } => Some(*started_at),
                DeferredHookOrchestratorStateV1::Unavailable { .. } => None,
            };
            if let Some(started_at) = started_at {
                *state = DeferredHookOrchestratorStateV1::Unavailable {
                    started_at,
                    reason: AdvisoryRuntimeUnavailableReasonV1::Cancelled,
                    finished_at,
                };
            }
        }
        let setup_task = { self.setup_task.lock().await.take() };
        if let Some(task) = setup_task {
            let _ = task.await;
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl HookOrchestrationPortV1 for DeferredHookOrchestratorV1 {
    fn admit(&self, request: HookOrchestrationRequestV1) -> HookOrchestrationAdmissionV1 {
        match self.readiness() {
            AdvisoryRuntimeReadinessV1::Warming { .. } => {
                return HookOrchestrationAdmissionV1::Warming;
            }
            AdvisoryRuntimeReadinessV1::Unavailable { .. } => {
                return HookOrchestrationAdmissionV1::Unavailable;
            }
            AdvisoryRuntimeReadinessV1::Ready { .. } => {}
        }
        let runtime = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*state {
                DeferredHookOrchestratorStateV1::Ready { runtime, .. } => Arc::clone(runtime),
                DeferredHookOrchestratorStateV1::Warming { .. }
                | DeferredHookOrchestratorStateV1::Unavailable { .. } => {
                    return HookOrchestrationAdmissionV1::Unavailable;
                }
            }
        };
        runtime.admit(request)
    }
}

pub(in crate::daemon::service) struct RegisteredHookOrchestrationRuntimeV1 {
    project_id: [u8; 16],
    worktree_id: [u8; 16],
    runtime: Arc<DeferredHookOrchestratorV1>,
}

impl RegisteredHookOrchestrationRuntimeV1 {
    pub(in crate::daemon::service) fn new(
        project_id: [u8; 16],
        worktree_id: [u8; 16],
        runtime: Arc<DeferredHookOrchestratorV1>,
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

    pub(in crate::daemon::service) fn runtime(&self) -> Arc<DeferredHookOrchestratorV1> {
        Arc::clone(&self.runtime)
    }
}

impl Drop for RegisteredHookOrchestrationRuntimeV1 {
    fn drop(&mut self) {
        self.runtime.cancel();
    }
}

type HookOrchestrationRegistryKey = ([u8; 16], [u8; 16]);
type HookOrchestrationRegistry =
    StdMutex<BTreeMap<HookOrchestrationRegistryKey, Weak<dyn HookOrchestrationPortV1>>>;

pub(super) fn hook_orchestration_registry() -> &'static HookOrchestrationRegistry {
    static REGISTRY: OnceLock<HookOrchestrationRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub(crate) fn admit_registered_hook_orchestration(
    envelope: HookEventEnvelopeV2,
    binding: HookScopeBindingV1,
    lifecycle: Option<ContextScoutLifecycleAddressV1>,
    configuration_revision: u64,
    explicit: bool,
    completion: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
) -> HookOrchestrationAdmissionV1 {
    let Some(mut request) = HookOrchestrationRequestV1::from_envelope(
        envelope,
        &binding,
        lifecycle,
        configuration_revision,
        explicit,
    ) else {
        return HookOrchestrationAdmissionV1::UnsupportedTrigger;
    };
    let Some(runtime) = hook_orchestration_registry()
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
        return HookOrchestrationAdmissionV1::Unavailable;
    };
    request.completion = completion;
    runtime.admit(request)
}

pub(in crate::daemon::service) struct SwitchableFeedbackCycleRuntimeV1 {
    current: RwLock<Arc<dyn FeedbackCycleRuntimePort>>,
}

pub(in crate::daemon) fn observe_accepted_feedback_cycle_terminal(
    observations: &Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>,
    project_id: &ProjectId,
    request: &FeedbackCycleRequest,
    outcome: FeedbackOutcomeV1,
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
        FeedbackSourceEventV1::Delivery {
            operation: FeedbackOperationV1::FeedbackCycle,
            route: FeedbackDeliveryRouteV1::Lsp,
            outcome,
            item_count: 0,
            duration_micros: None,
        },
    );
}

pub(in crate::daemon::service) struct UnavailableFeedbackCycleRuntimeV1 {
    project_id: ProjectId,
    observations: Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>,
}

impl UnavailableFeedbackCycleRuntimeV1 {
    pub(in crate::daemon::service) fn new(
        project_id: ProjectId,
        observations: Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>,
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
                FeedbackOutcomeV1::Unavailable,
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
    ) -> Result<Arc<dyn FeedbackCycleRuntimePort>, LspRuntimeFailure> {
        let mut guard = self
            .current
            .write()
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-router"))?;
        Ok(std::mem::replace(&mut *guard, current))
    }

    /// Restores `replacement` only while `expected` is still routed, so a
    /// rollback can never clobber a successor publication.
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
    pub(super) actor: ActorId,
    pub(super) grant: CapabilityGrantSnapshot,
    pub(super) authority_digest: ManifestDigest,
    pub(super) policy_digest: ManifestDigest,
    pub(super) configuration_digest: ManifestDigest,
}

pub(in crate::daemon::service) struct RegisteredFeedbackRuntime {
    pub(super) project_id: ProjectId,
    pub(super) runtime: Arc<FeedbackRuntime>,
}

impl RegisteredFeedbackRuntime {
    pub(in crate::daemon::service) fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub(in crate::daemon::service) fn runtime(&self) -> Arc<FeedbackRuntime> {
        Arc::clone(&self.runtime)
    }

    pub(in crate::daemon::service) fn invocation_owner(&self) -> DaemonFeedbackInvocationOwner {
        DaemonFeedbackInvocationOwner::new(self.project_id.clone(), self.runtime.owner())
    }

    pub(in crate::daemon::service) fn source_observation_port(
        &self,
    ) -> Arc<dyn FeedbackObservationEmitterV1 + Send + Sync> {
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
    pub(super) semantic_evaluation_workers:
        Arc<crate::daemon::semantic_evaluation::DaemonSemanticEvaluationWorkerOwnerV1>,
}

impl RegisteredConfigurationRuntime {
    pub(in crate::daemon::service) fn semantic_evaluation_workers(
        &self,
    ) -> &Arc<crate::daemon::semantic_evaluation::DaemonSemanticEvaluationWorkerOwnerV1> {
        &self.semantic_evaluation_workers
    }
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
