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
type AdvisoryHookOrchestrationWorkV1 =
    dyn Fn(AdvisoryHookOrchestrationRequestV1) -> AdvisoryHookOrchestrationFutureV1 + Send + Sync;
type AdvisoryHookOrchestrationKeyV1 = ([u8; 16], [u8; 16], [u8; 16]);
type AdvisoryHookOrchestrationCompletionV1 = Arc<dyn Fn() + Send + Sync + 'static>;
type AdvisoryHookOrchestrationInFlightV1 =
    StdMutex<BTreeMap<AdvisoryHookOrchestrationKeyV1, Vec<AdvisoryHookOrchestrationCompletionV1>>>;
pub(in crate::daemon::service) const MAX_COALESCED_ADVISORY_HOOK_COMPLETIONS: usize = 32;

pub(crate) struct BoundedAdvisoryHookOrchestratorV1 {
    permits: Arc<Semaphore>,
    work: Arc<AdvisoryHookOrchestrationWorkV1>,
    in_flight: Arc<AdvisoryHookOrchestrationInFlightV1>,
    cancellation: crate::application::context::CancellationToken,
}

impl BoundedAdvisoryHookOrchestratorV1 {
    pub(crate) fn new<F, Fut>(max_concurrent: usize, work: F) -> Option<Arc<Self>>
    where
        F: Fn(AdvisoryHookOrchestrationRequestV1) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let work: Arc<AdvisoryHookOrchestrationWorkV1> =
            Arc::new(move |request| Box::pin(work(request)));
        (max_concurrent > 0).then(|| {
            Arc::new(Self {
                permits: Arc::new(Semaphore::new(max_concurrent)),
                work,
                in_flight: Arc::new(StdMutex::new(BTreeMap::new())),
                cancellation: crate::application::context::CancellationToken::new(),
            })
        })
    }
}

impl AdvisoryHookOrchestrationPortV1 for BoundedAdvisoryHookOrchestratorV1 {
    fn admit(
        &self,
        mut request: AdvisoryHookOrchestrationRequestV1,
    ) -> AdvisoryHookOrchestrationAdmissionV1 {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
        };
        let envelope = request.hook.envelope();
        let key = (envelope.project_id, envelope.worktree_id, envelope.event_id);
        let completion = request.completion.take();
        let permit = {
            let Ok(mut in_flight) = self.in_flight.lock() else {
                return AdvisoryHookOrchestrationAdmissionV1::Unavailable;
            };
            if let Some(completions) = in_flight.get_mut(&key) {
                if let Some(completion) = completion {
                    if completions.len() >= MAX_COALESCED_ADVISORY_HOOK_COMPLETIONS {
                        return AdvisoryHookOrchestrationAdmissionV1::Backpressured;
                    }
                    completions.push(completion);
                }
                return AdvisoryHookOrchestrationAdmissionV1::Enqueued;
            }
            let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
                return AdvisoryHookOrchestrationAdmissionV1::Backpressured;
            };
            in_flight.insert(key, completion.into_iter().collect());
            permit
        };
        let work = Arc::clone(&self.work);
        let in_flight = Arc::clone(&self.in_flight);
        let cancellation = self.cancellation.clone();
        handle.spawn(async move {
            let completed = tokio::select! {
                () = cancellation.cancelled() => false,
                () = (work)(request) => true,
            };
            let completions = in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key);
            match completions {
                Some(completions) if completed => {
                    for completion in completions {
                        completion();
                    }
                }
                Some(_) => {}
                None => tracing::error!(
                    event = "advisory_hook_orchestration_completion_missing",
                    "daemon-owned advisory work finished without its in-flight admission"
                ),
            }
            drop(permit);
        });
        AdvisoryHookOrchestrationAdmissionV1::Enqueued
    }
}

impl Drop for BoundedAdvisoryHookOrchestratorV1 {
    fn drop(&mut self) {
        self.cancellation.cancel();
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
    cancellation: crate::application::context::CancellationToken,
}

impl DeferredAdvisoryHookOrchestratorV1 {
    pub(crate) fn new(started_at: UtcMicros) -> Arc<Self> {
        Arc::new(Self {
            started_at,
            state: StdMutex::new(DeferredAdvisoryHookOrchestratorStateV1::Warming),
            setup_claimed: AtomicBool::new(false),
            cancellation: crate::application::context::CancellationToken::new(),
        })
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
        *self
            .current
            .write()
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-router"))? = current;
        Ok(())
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
