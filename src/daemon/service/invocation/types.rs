//! Shared retained-state shapes and small daemon-private types used across the invocation split.

use super::*;
use tracedecay_application::RegisteredRootLocatorV1;
use tracedecay_runtime_core::cancellation::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookOrchestrationAdmissionV1 {
    Enqueued,
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

/// Terminal state of one executed hook cycle. Only `Completed` acknowledges
/// the admissions joined to the cycle; `RetainForReplay` leaves their durable
/// pending-work records in place so the daemon replay consumer retries the
/// producer work after the unavailable input recovers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HookOrchestrationWorkOutcomeV1 {
    Completed,
    RetainForReplay,
}

type HookOrchestrationFutureV1 =
    Pin<Box<dyn Future<Output = HookOrchestrationWorkOutcomeV1> + Send + 'static>>;
type HookOrchestrationWorkV1 = dyn Fn(HookOrchestrationRequestV1, CancellationToken) -> HookOrchestrationFutureV1
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
    cancellation: CancellationToken,
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

/// Bounded owner of one project's advisory-and-Scout hook cycles: exact
/// duplicate boundaries join the running cycle, a newer boundary at the same
/// stable session address supersedes the incumbent, and everything beyond the
/// permit and coalescing bounds is backpressured instead of queued.
pub(crate) struct BoundedHookOrchestratorV1 {
    permits: Arc<Semaphore>,
    work: Arc<HookOrchestrationWorkV1>,
    in_flight: Arc<StdMutex<HookOrchestrationInFlightV1>>,
    cancellation: CancellationToken,
}

impl BoundedHookOrchestratorV1 {
    pub(crate) fn new<F, Fut>(max_concurrent: usize, work: F) -> Option<Arc<Self>>
    where
        F: Fn(HookOrchestrationRequestV1, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOrchestrationWorkOutcomeV1> + Send + 'static,
    {
        let work: Arc<HookOrchestrationWorkV1> =
            Arc::new(move |request, cancellation| Box::pin(work(request, cancellation)));
        (max_concurrent > 0).then(|| {
            Arc::new(Self {
                permits: Arc::new(Semaphore::new(max_concurrent)),
                work,
                in_flight: Arc::new(StdMutex::new(HookOrchestrationInFlightV1::default())),
                cancellation: CancellationToken::new(),
            })
        })
    }

    fn stable_address(request: &HookOrchestrationRequestV1) -> Option<HookOrchestrationAddressV1> {
        let envelope = request.hook.envelope();
        // Only session-stable lifecycle identity enters the address: turn and
        // message identifiers advance on every native tool call, and a newer
        // boundary must land on the incumbent's exact address to supersede it.
        let lifecycle = request.lifecycle.as_ref().map(|lifecycle| {
            (
                &lifecycle.profile_id,
                &lifecycle.provider_id,
                &lifecycle.session_id,
            )
        });
        canonical_sha256(&(
            "tracedecay.advisory-hook-address.v1",
            envelope.project_id,
            envelope.repository_id,
            envelope.worktree_id,
            envelope.protected_session_id,
            lifecycle,
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

    pub(crate) fn admit(
        &self,
        mut request: HookOrchestrationRequestV1,
    ) -> HookOrchestrationAdmissionV1 {
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
            let work_cancellation = CancellationToken::new();
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
            // Only genuinely completed or superseded-and-settled work emits
            // terminals. A cycle that ends in `RetainForReplay` keeps its
            // joined admissions' durable pending-work records so the replay
            // consumer retries the producer work. Supersession joins the
            // cancelled cycle to completion — nested blocking segments must
            // settle before the receipt terminal is emitted — while owner
            // retirement drops the work future without a terminal: pending
            // work must stop when its owner retires, and silence is a normal
            // result that no adapter may replace with an invented reason.
            let outcome = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    work_cancellation.cancel();
                    None
                },
                () = work_cancellation.cancelled() => {
                    if operation
                        .superseded
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        tokio::select! {
                            biased;
                            () = cancellation.cancelled() => None,
                            outcome = &mut work_future => Some(outcome),
                        }
                    } else {
                        None
                    }
                },
                outcome = &mut work_future => Some(outcome),
            };
            let superseded = operation
                .superseded
                .load(std::sync::atomic::Ordering::Acquire);
            let emit_terminal = match outcome {
                Some(HookOrchestrationWorkOutcomeV1::Completed) => true,
                // The successor acknowledges the settled obsolete cycle even
                // when it asked for replay: the newer boundary at the same
                // address owns the session's current state.
                Some(HookOrchestrationWorkOutcomeV1::RetainForReplay) => superseded,
                None => {
                    drop(work_future);
                    false
                }
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

type HookOrchestrationRegistryKey = ([u8; 16], [u8; 16]);
type HookOrchestrationRegistry =
    StdMutex<BTreeMap<HookOrchestrationRegistryKey, Weak<BoundedHookOrchestratorV1>>>;

fn hook_orchestration_registry() -> &'static HookOrchestrationRegistry {
    static REGISTRY: OnceLock<HookOrchestrationRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

/// Publishes one project's hook orchestrator under its authenticated hook
/// locators. A different live incumbent keeps the key: admission must never
/// route one project's boundaries at another project's cycle owner.
pub(crate) fn register_hook_orchestration_runtime(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    runtime: &Arc<BoundedHookOrchestratorV1>,
) -> bool {
    let key = (hook_project_id, hook_worktree_id);
    let Ok(mut registry) = hook_orchestration_registry().lock() else {
        return false;
    };
    if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
        return Arc::ptr_eq(&existing, runtime);
    }
    registry.retain(|_, runtime| runtime.strong_count() > 0);
    registry.insert(key, Arc::downgrade(runtime));
    true
}

/// Removes exactly the given orchestrator's registration; a different live
/// runtime under the same locator pair is left untouched so a rolled-back
/// setup can never unregister its successor.
pub(crate) fn unregister_hook_orchestration_runtime(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    runtime: &Arc<BoundedHookOrchestratorV1>,
) {
    let key = (hook_project_id, hook_worktree_id);
    if let Ok(mut registry) = hook_orchestration_registry().lock()
        && registry
            .get(&key)
            .and_then(Weak::upgrade)
            .is_some_and(|existing| Arc::ptr_eq(&existing, runtime))
    {
        registry.remove(&key);
    }
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
    pub(super) database: crate::global_db::RegisteredGlobalDbLeaseV1,
    pub(super) actor: ActorId,
    pub(super) grant: CapabilityGrantSnapshot,
    pub(super) authority_digest: ManifestDigest,
    pub(super) policy_digest: ManifestDigest,
    pub(super) configuration_digest: ManifestDigest,
    /// The complete resolved work topology policy pinned at registration;
    /// workflow run admission and placement evaluate against this policy.
    pub(super) work_topology_policy: tracedecay_domain::configuration::WorkTopologyPolicyV1,
    /// Project-open-pinned proposal routing authority over the exact admitted
    /// configuration snapshot and executable bindings.
    pub(super) proposal_routing: super::work_routing::DaemonWorkProposalRoutingAuthorityV1,
    /// Canonical Plan-23 adapter with per-request evaluated-profile resolution.
    pub(super) evidence_retrieval:
        crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1,
    /// Project-owned bounded replay for receipts that closed outside a request
    /// response, such as terminal attempt compare-and-swaps.
    pub(super) blocked_interval_observation_recovery:
        super::work_blocked_interval_recovery::WorkBlockedIntervalObservationRecoveryOwnerV1,
    /// Project-owned bounded durable recovery for exact workflow topology
    /// census observations, including terminal intervals after restart.
    pub(super) workflow_census_observation_recovery:
        super::work::workflow_census::WorkflowFanOutCensusObservationRecoveryOwnerV1,
    /// Retained bounded restart reconciliation for active workflow fan-out
    /// runs. `None` exists only while the runtime value used by the owner is
    /// assembled; every published runtime retains a mounted owner.
    pub(super) workflow_fan_out_recovery:
        Option<super::work::workflow_fan_out::WorkflowFanOutRecoveryOwnerV1>,
}

impl RegisteredWorkRuntime {
    pub(in crate::daemon::service) async fn shut_down_background_recovery(&self) {
        if let Some(recovery) = &self.workflow_fan_out_recovery {
            recovery.shutdown().await;
        }
        self.blocked_interval_observation_recovery.shutdown().await;
        self.workflow_census_observation_recovery.shutdown().await;
    }
}

#[derive(Clone)]
pub(in crate::daemon::service) struct RegisteredRetainedRuntime {
    pub(super) scope: ResolvedScope,
    pub(super) actor: ActorId,
    pub(super) grant: CapabilityGrantSnapshot,
    pub(super) ports:
        Arc<tracedecay_application::retained_surfaces::RetainedSurfacePortsV1<'static>>,
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct InvocationProjectRuntimeIdentityV1 {
    profile_id: UserProfileId,
    project_id: ProjectId,
    project_root: PathBuf,
}

impl InvocationProjectRuntimeIdentityV1 {
    pub(super) fn new(
        profile_id: UserProfileId,
        project_id: ProjectId,
        project_root: PathBuf,
    ) -> Self {
        Self {
            profile_id,
            project_id,
            project_root,
        }
    }

    pub(super) fn belongs_to(
        &self,
        profile_id: &UserProfileId,
        project_id: &ProjectId,
        project_roots: &std::collections::BTreeSet<PathBuf>,
    ) -> bool {
        &self.profile_id == profile_id
            && &self.project_id == project_id
            && project_roots.contains(&self.project_root)
    }

    pub(super) fn matches_locator(&self, locator: &RegisteredRootLocatorV1) -> bool {
        self.profile_id == locator.profile.profile_id
            && self.project_id == locator.project_id
            && self.project_root == locator.canonical_root
    }

    pub(super) fn matches_project_root(&self, project_root: &Path) -> bool {
        self.project_root == project_root
    }
}

#[derive(Clone)]
pub(in crate::daemon::service) struct RegisteredConfigurationRuntime {
    pub(super) runtime: Arc<ProjectConfigurationRuntime>,
    pub(super) scope: ResolvedScope,
    pub(super) project_identity: InvocationProjectRuntimeIdentityV1,
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
    pub(super) project_identity: InvocationProjectRuntimeIdentityV1,
    pub(super) actor: RuntimeLspActor,
    pub(super) delivery_settlements:
        Option<Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>>,
    /// Captured at the first poll of the current outbound frame. Retries and
    /// terminalization must reuse its exact timestamps and identity.
    pub(super) in_flight_delivery_attempt: Option<tracedecay_domain::DeliverySettlementAttemptV1>,
    /// Each queued outbound occurrence receives a unique session-local event
    /// sequence when first polled; retries retain the already captured attempt.
    pub(super) next_delivery_sequence: u64,
}

struct LspLeaseTask {
    generation: u64,
    cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
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
            let cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
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
        if let Some(previous) = previous
            && previous.stop().await.is_err()
        {
            self.stop_generation(&current_session_id, Some(generation))
                .await?;
            return Err(DaemonInvocationProblem::Unavailable);
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
    pub(super) project_identity: InvocationProjectRuntimeIdentityV1,
    pub(super) factory: Arc<DaemonLspSessionFactory>,
    pub(super) scope_grant: Option<CapabilityGrantSnapshot>,
    pub(super) scope_set_storage:
        Option<tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage>,
    pub(super) delivery_settlements:
        Option<Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>>,
}

#[derive(Clone)]
pub(super) struct AuthorizedDaemonLspWorkspace {
    pub(super) scope_set: AuthorizedScopeSet,
    pub(super) factories: Vec<(AdmittedRoot, Arc<DaemonLspSessionFactory>)>,
}

impl DaemonLspInvocationOwner {
    #[cfg(test)]
    pub(crate) fn new(factory: Arc<DaemonLspSessionFactory>) -> Self {
        Self::for_test_project(
            factory,
            UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            ProjectId::new("project.test.lsp").expect("test LSP project"),
            PathBuf::from("/test/lsp"),
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test_project(
        factory: Arc<DaemonLspSessionFactory>,
        profile_id: UserProfileId,
        project_id: ProjectId,
        project_root: PathBuf,
    ) -> Self {
        Self {
            project_identity: InvocationProjectRuntimeIdentityV1::new(
                profile_id,
                project_id,
                project_root,
            ),
            factory,
            scope_grant: None,
            scope_set_storage: None,
            delivery_settlements: None,
        }
    }

    pub(super) fn authorized(
        project_identity: InvocationProjectRuntimeIdentityV1,
        factory: Arc<DaemonLspSessionFactory>,
        scope_grant: CapabilityGrantSnapshot,
        scope_set_storage: tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage,
        delivery_settlements: Arc<
            tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1,
        >,
    ) -> Self {
        Self {
            project_identity,
            factory,
            scope_grant: Some(scope_grant),
            scope_set_storage: Some(scope_set_storage),
            delivery_settlements: Some(delivery_settlements),
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
