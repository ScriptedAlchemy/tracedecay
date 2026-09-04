//! Per-subsystem `*RuntimeRegistrar` newtypes and their registration error enums.

use super::*;

mod lsp;
pub use lsp::DaemonLspOwnerRegistrar;

#[derive(Clone)]
pub struct DaemonConfigurationGrantAuthority {
    actor: ActorId,
    pub(super) policy_epoch: u64,
    pub(super) policy_digest: AccessPolicyDigest,
    pub(super) expires_at: UtcMicros,
    direct_layers: Arc<BTreeMap<ManifestDigest, ConfigurationLayerIdV1>>,
    grants: Arc<RwLock<BTreeMap<ConfigurationGrantId, ConfigurationMutationGrantSnapshotV1>>>,
}

impl DaemonConfigurationGrantAuthority {
    pub fn issue_direct(
        &self,
        request_id: &str,
        idempotency_key: ConfigurationIdempotencyKey,
        mutation: &DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
        effective_deadline_at: UtcMicros,
        issued_at: UtcMicros,
    ) -> Result<ConfigurationMutationAuthority, DaemonInvocationProblem> {
        let layer = mutation
            .target_layer()
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let scope_digest = mutation
            .target_scope_digest()
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        if self.direct_layers.get(&scope_digest) != Some(layer) {
            return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        self.issue(
            request_id,
            ConfigurationMutationOperationV1::DirectMutation,
            scope_digest,
            expected_revision,
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CommitConfigurationRevision,
            Some(idempotency_key),
            effective_deadline_at,
            issued_at,
        )
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn for_test(
        layers: impl IntoIterator<Item = ConfigurationLayerIdV1>,
        expires_at: UtcMicros,
    ) -> Option<Self> {
        Some(Self {
            actor: ActorId::new("actor.configuration.test").ok()?,
            policy_epoch: 1,
            policy_digest: AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64))).ok()?,
            expires_at,
            direct_layers: Arc::new(
                layers
                    .into_iter()
                    .map(|layer| {
                        configuration_layer_scope_digest(&layer)
                            .ok()
                            .map(|digest| (digest, layer))
                    })
                    .collect::<Option<BTreeMap<_, _>>>()?,
            ),
            grants: Arc::new(RwLock::new(BTreeMap::new())),
        })
    }

    pub(super) fn issue(
        &self,
        request_id: &str,
        operation: ConfigurationMutationOperationV1,
        scope_digest: ManifestDigest,
        expected_revision: ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        idempotency_key: Option<ConfigurationIdempotencyKey>,
        effective_deadline_at: UtcMicros,
        issued_at: UtcMicros,
    ) -> Result<ConfigurationMutationAuthority, DaemonInvocationProblem> {
        let expires_at = UtcMicros(self.expires_at.0.min(effective_deadline_at.0));
        if issued_at >= expires_at {
            return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        let grant_id = ConfigurationGrantId::new(format!("configuration.grant.{request_id}"))
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let receipt_id =
            ConfigurationGrantReceiptId::new(format!("configuration.grant-receipt.{request_id}"))
                .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let permission = ConfigurationMutationPermissionV1 {
            operation,
            sink,
            effect,
        };
        let grant_digest = canonical_sha256(&(
            "tracedecay.daemon.configuration-grant.v1",
            &grant_id,
            &self.actor,
            &scope_digest,
            &expected_revision,
            permission,
            self.policy_epoch,
            &self.policy_digest,
            issued_at,
            expires_at,
        ))
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        let receipt = ConfigurationMutationGrantReceiptV1::issue(
            receipt_id,
            grant_id.clone(),
            self.actor.clone(),
            operation,
            scope_digest.clone(),
            expected_revision.clone(),
            self.policy_epoch,
            self.policy_digest.clone(),
            sink,
            effect,
            idempotency_key,
            issued_at,
            expires_at,
        )
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        let snapshot = ConfigurationMutationGrantSnapshotV1 {
            grant_id: grant_id.clone(),
            grant_revision: 1,
            grant_digest,
            authorized_receipt_digest: receipt.receipt_digest.clone(),
            actor_id: self.actor.clone(),
            scope_digest: scope_digest.clone(),
            expected_configuration_revision: expected_revision.clone(),
            permissions: std::collections::BTreeSet::from([permission]),
            policy_epoch: self.policy_epoch,
            policy_digest: self.policy_digest.clone(),
            issued_at,
            expires_at,
            state: ConfigurationMutationGrantStateV1::Active,
        };
        if !snapshot.is_valid() {
            return Err(DaemonInvocationProblem::Unavailable);
        }
        self.grants
            .write()
            .map_err(|_| DaemonInvocationProblem::Unavailable)?
            .insert(grant_id, snapshot);
        Ok(ConfigurationMutationAuthority { receipt })
    }
}

pub fn mounted_configuration_layers(
    project_id: &ProjectId,
    profile_id: &UserProfileId,
    snapshot: &ConfigurationSnapshotV1,
) -> Result<BTreeMap<ManifestDigest, ConfigurationLayerIdV1>, DaemonInvocationProblem> {
    let mut layers = std::collections::BTreeSet::from([
        ConfigurationLayerIdV1::Project {
            project_id: project_id.clone(),
        },
        ConfigurationLayerIdV1::UserProfile {
            profile_id: profile_id.clone(),
        },
    ]);
    layers.extend(
        snapshot
            .provenance
            .values()
            .flatten()
            .filter_map(|candidate| match &candidate.layer {
                ConfigurationLayerIdV1::Collection { .. }
                    if matches!(
                        candidate.disposition,
                        CandidateDispositionV1::Winning | CandidateDispositionV1::Defaulted
                    ) =>
                {
                    Some(candidate.layer.clone())
                }
                _ => None,
            }),
    );
    layers
        .into_iter()
        .map(|layer| {
            configuration_layer_scope_digest(&layer)
                .map(|digest| (digest, layer))
                .map_err(|_| DaemonInvocationProblem::Unavailable)
        })
        .collect()
}

impl ConfigurationMutationGrantAuthority for DaemonConfigurationGrantAuthority {
    fn current_grant<'a>(
        &'a self,
        grant_id: &'a ConfigurationGrantId,
    ) -> ConfigurationMutationGrantAuthorityFuture<'a> {
        let result = self
            .grants
            .read()
            .map_err(|_| ConfigurationMutationGrantAuthorityError::Unavailable)
            .and_then(|grants| {
                grants
                    .get(grant_id)
                    .cloned()
                    .ok_or(ConfigurationMutationGrantAuthorityError::Rejected)
            });
        Box::pin(async move { result })
    }
}

#[derive(Clone)]
struct DaemonConfigurationScopeResolution {
    actor: ActorId,
    evidence: ScopeRevalidationEvidenceV1,
}

impl ScopeResolutionPort for DaemonConfigurationScopeResolution {
    fn resolve_protected_change<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        change: &'a tracedecay_domain::configuration::ProtectedChange,
    ) -> tracedecay_configuration::ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1>
    {
        let allowed = actor.actor_id == self.actor && change.validate().is_ok();
        let evidence = self.evidence.clone();
        Box::pin(async move {
            allowed
                .then_some(evidence)
                .ok_or(tracedecay_configuration::ConfigurationError::TargetUnavailable)
        })
    }

    fn revalidate_plan<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        plan: &'a tracedecay_domain::configuration::ProtectedChangePlan,
    ) -> tracedecay_configuration::ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1>
    {
        let allowed = actor.actor_id == self.actor && plan.validate().is_ok();
        let evidence = self.evidence.clone();
        Box::pin(async move {
            allowed
                .then_some(evidence)
                .ok_or(tracedecay_configuration::ConfigurationError::TargetUnavailable)
        })
    }
}

#[derive(Debug, Error)]
pub enum DaemonFeedbackRuntimeRegistrationError {
    #[error("a feedback runtime is already mounted for this project database")]
    AlreadyRegistered,
    #[error("the daemon project runtime registry is closed")]
    RegistryClosed,
    #[error("a concurrent feedback runtime build failed: {detail}")]
    ConcurrentBuildFailed { detail: String },
    #[error("the shared feedback runtime is not mounted for this project")]
    MissingRuntime,
    #[error("the switchable feedback-cycle route could not be published")]
    CyclePublication,
    #[error("the feedback runtime could not be opened")]
    Runtime(#[from] FeedbackRuntimeError),
    #[error("the feedback cycle runtime could not be opened")]
    Cycle(#[from] FeedbackCycleRuntimeError),
    #[error("the policy evaluator composition is invalid")]
    Policy(#[from] ApplicationContractError),
}

impl From<FeedbackCyclePublicationError> for DaemonFeedbackRuntimeRegistrationError {
    fn from(error: FeedbackCyclePublicationError) -> Self {
        match error {
            FeedbackCyclePublicationError::Registry(
                ProjectRuntimeRegistryError::AlreadyRegistered,
            ) => Self::AlreadyRegistered,
            FeedbackCyclePublicationError::Registry(ProjectRuntimeRegistryError::Closed) => {
                Self::RegistryClosed
            }
            FeedbackCyclePublicationError::Registry(
                ProjectRuntimeRegistryError::ConcurrentBuildFailed { detail },
            ) => Self::ConcurrentBuildFailed { detail },
            FeedbackCyclePublicationError::RouterUnavailable => Self::CyclePublication,
        }
    }
}

impl From<ProjectRuntimeAlreadyRegistered> for DaemonFeedbackRuntimeRegistrationError {
    fn from(_: ProjectRuntimeAlreadyRegistered) -> Self {
        Self::AlreadyRegistered
    }
}

/// Maps registry refusals into one per-runtime registration enum.
/// Callers match on the per-runtime variants (and each carries its own error
/// message), so the enums keep their own `AlreadyRegistered`/`RegistryClosed`
/// shapes and only this mapping is shared.
pub(super) fn registry_registration_refusal<E>(
    error: ProjectRuntimeRegistryError,
    already_registered: E,
    registry_closed: E,
    concurrent_build_failed: impl FnOnce(String) -> E,
) -> E {
    match error {
        ProjectRuntimeRegistryError::AlreadyRegistered => already_registered,
        ProjectRuntimeRegistryError::Closed => registry_closed,
        ProjectRuntimeRegistryError::ConcurrentBuildFailed { detail } => {
            concurrent_build_failed(detail)
        }
    }
}

impl From<ProjectRuntimeRegistryError> for DaemonFeedbackRuntimeRegistrationError {
    fn from(error: ProjectRuntimeRegistryError) -> Self {
        registry_registration_refusal(
            error,
            Self::AlreadyRegistered,
            Self::RegistryClosed,
            |detail| Self::ConcurrentBuildFailed { detail },
        )
    }
}

impl From<ProjectRuntimeRegistryError> for TraceDecayError {
    fn from(error: ProjectRuntimeRegistryError) -> Self {
        Self::Config {
            message: match error {
                ProjectRuntimeRegistryError::AlreadyRegistered => {
                    "a project runtime component is already registered".to_owned()
                }
                ProjectRuntimeRegistryError::Closed => {
                    "the daemon project runtime registry is closed".to_owned()
                }
                ProjectRuntimeRegistryError::ConcurrentBuildFailed { detail } => {
                    format!("a concurrent project runtime component build failed: {detail}")
                }
            },
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub struct DaemonFeedbackPublicationTestGate {
    publication_ready: StdMutex<Option<tokio::sync::oneshot::Sender<()>>>,
    continue_publication: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl DaemonFeedbackPublicationTestGate {
    pub fn new(
        publication_ready: tokio::sync::oneshot::Sender<()>,
        continue_publication: tokio::sync::oneshot::Receiver<()>,
    ) -> Self {
        Self {
            publication_ready: StdMutex::new(Some(publication_ready)),
            continue_publication: Mutex::new(Some(continue_publication)),
        }
    }

    #[hotpath::skip]
    async fn wait(&self) -> bool {
        let Some(publication_ready) = self
            .publication_ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return false;
        };
        if publication_ready.send(()).is_err() {
            return false;
        }
        let Some(continue_publication) = self.continue_publication.lock().await.take() else {
            return false;
        };
        continue_publication.await.is_ok()
    }
}

#[derive(Clone)]
pub struct DaemonFeedbackRuntimeRegistrar {
    service: DaemonInvocationService,
    #[cfg(any(test, feature = "test-helpers"))]
    pub producer_constructions: Arc<AtomicUsize>,
    #[cfg(any(test, feature = "test-helpers"))]
    publication_gate: Option<Arc<DaemonFeedbackPublicationTestGate>>,
}

impl DaemonFeedbackRuntimeRegistrar {
    pub fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
            #[cfg(any(test, feature = "test-helpers"))]
            producer_constructions: Arc::new(AtomicUsize::new(0)),
            #[cfg(any(test, feature = "test-helpers"))]
            publication_gate: None,
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[must_use]
    pub fn with_publication_gate(mut self, gate: Arc<DaemonFeedbackPublicationTestGate>) -> Self {
        self.publication_gate = Some(gate);
        self
    }

    /// Resolve the read store from the feedback runtime mounted for this exact
    /// project root. Doctor receives no provider runtime or write authority.
    #[hotpath::skip]
    pub async fn doctor_read_store(&self, project_root: &Path) -> Option<ProjectFeedbackStore> {
        self.service
            .feedback_runtime(Some(project_root))
            .await
            .map(|runtime| runtime.publication_store())
    }

    /// Registers feedback readers from the authoritative admission result.
    #[hotpath::skip]
    pub async fn open_and_register(
        &self,
        database: Database,
        project_root: PathBuf,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
        authorization: Arc<dyn CallableCodeAuthorizationSourcePort>,
    ) -> Result<ProjectFeedbackStore, DaemonFeedbackRuntimeRegistrationError> {
        let runtime_root = project_root.clone();
        #[cfg(any(test, feature = "test-helpers"))]
        let producer_constructions = Arc::clone(&self.producer_constructions);
        #[cfg(any(test, feature = "test-helpers"))]
        let publication_gate = self.publication_gate.clone();
        self.service
            .project_runtimes
            .publish_feedback_atomically(project_root, move |mut publication| async move {
                let project_id = scope.project_id.clone();
                #[cfg(any(test, feature = "test-helpers"))]
                producer_constructions.fetch_add(1, Ordering::SeqCst);
                let runtime = Arc::new(
                    open_feedback_runtime(database, runtime_root.clone(), scope.clone(), access)
                        .await?,
                );
                let publications = runtime.publication_store();
                let unavailable_cycle = Arc::new(UnavailableFeedbackCycleRuntimeV1::new(
                    project_id.clone(),
                    runtime.source_observation_port(),
                ));
                publication.stage(RegisteredCallableCodeRuntime {
                    scope,
                    authorization,
                })?;
                publication.stage(RegisteredFeedbackRuntime {
                    project_id,
                    runtime,
                })?;
                publication.stage(Arc::new(SwitchableFeedbackCycleRuntimeV1::new(
                    unavailable_cycle,
                )))?;
                #[cfg(any(test, feature = "test-helpers"))]
                if let Some(gate) = publication_gate
                    && !gate.wait().await
                {
                    return Err(DaemonFeedbackRuntimeRegistrationError::CyclePublication);
                }
                Ok((publication, publications))
            })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::skip]
    pub async fn open_cycle_and_register(
        &self,
        project_root: PathBuf,
        database: Database,
        runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
        policy_context: PolicyEvaluationContextV1,
        evidence_horizon: PolicyEvidenceHorizonV1,
        evaluated_at: UtcMicros,
        provider_candidates: Vec<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1)>,
        code_graph: Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort>,
        affected_tests: Arc<dyn AffectedTestsRetrievalPort + Send + Sync>,
        operation: ApplicationOperation,
        graph_operation: ApplicationOperation,
        tests_operation: ApplicationOperation,
        lsp_input: FeedbackCycleLspInput,
        proximity: Arc<dyn ProductionFeedbackCycleProximityPortV1>,
    ) -> Result<Arc<FeedbackCycleRuntime>, DaemonFeedbackRuntimeRegistrationError> {
        let policy = PolicyEvaluatorCompositionV1::from_application_catalog()?;
        let correlation_state = evidence_horizon.routing_state();
        let correlation_availability = match correlation_state {
            TruthSourceStateV1::Fresh | TruthSourceStateV1::Partial => {
                CapabilityAvailabilityV1::Available
            }
            TruthSourceStateV1::Stale => CapabilityAvailabilityV1::Stale,
            TruthSourceStateV1::Unavailable => CapabilityAvailabilityV1::Unavailable,
            TruthSourceStateV1::Unknown => CapabilityAvailabilityV1::Unknown,
        };
        let correlation_policy = operation.evaluate_local_live_policy(
            &policy,
            &policy_context,
            correlation_availability,
            ScopeMatchV1::Match,
            correlation_state,
            CapabilityEffectClassV1::Read,
            TruthFreshnessRequirementV1::FreshOrPartial,
            evidence_horizon,
            evaluated_at,
        )?;
        let provider_admissions = provider_candidates
            .into_iter()
            .map(|(identity, input)| {
                AnalyzerAdmittedDiagnosticProviderV1::evaluate_current_configuration_snapshot(
                    &policy,
                    &policy_context,
                    identity,
                    input,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let feedback = self
            .service
            .feedback_runtime(Some(&project_root))
            .await
            .ok_or(DaemonFeedbackRuntimeRegistrationError::MissingRuntime)?;
        let observations = feedback.observation_port();
        let production_lsp_input = Arc::clone(&lsp_input);
        let runtime = open_feedback_cycle_runtime(
            database,
            feedback,
            runtime_state,
            correlation_policy,
            provider_admissions,
            project_root.clone(),
            code_graph,
            affected_tests,
            observations,
            operation,
            graph_operation,
            tests_operation,
            lsp_input,
            Some(Arc::new(self.service.code_index_schedulers.clone())),
        )?;
        let production_input = production_proximity_feedback_cycle_input(
            Arc::clone(&runtime),
            production_lsp_input,
            proximity,
        );
        self.service
            .project_runtimes
            .publish_feedback_cycle_atomically(project_root, Arc::clone(&runtime), production_input)
            .await?;
        Ok(runtime)
    }
}

#[derive(Debug, Error)]
pub enum DaemonAdvisoryRuntimeRegistrationError {
    #[error("an advisory runtime is already mounted for this project")]
    AlreadyRegistered,
    #[error("the daemon project runtime registry is closed")]
    RegistryClosed,
    #[error("a concurrent advisory runtime build failed: {detail}")]
    ConcurrentBuildFailed { detail: String },
    #[error("the shared feedback cycle must be registered before advisory")]
    MissingFeedbackRuntime,
    #[error("the advisory production authorities could not be opened")]
    Production(#[from] AdvisoryProductionOpenErrorV1),
    #[error(transparent)]
    Startup(#[from] AdvisoryDaemonStartupErrorV1),
}

impl From<FeedbackCyclePublicationError> for DaemonAdvisoryRuntimeRegistrationError {
    fn from(error: FeedbackCyclePublicationError) -> Self {
        match error {
            FeedbackCyclePublicationError::Registry(registry) => registry.into(),
            FeedbackCyclePublicationError::RouterUnavailable => Self::MissingFeedbackRuntime,
        }
    }
}

impl From<ProjectRuntimeRegistryError> for DaemonAdvisoryRuntimeRegistrationError {
    fn from(error: ProjectRuntimeRegistryError) -> Self {
        registry_registration_refusal(
            error,
            Self::AlreadyRegistered,
            Self::RegistryClosed,
            |detail| Self::ConcurrentBuildFailed { detail },
        )
    }
}

#[derive(Clone)]
pub struct DaemonAdvisoryRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonAdvisoryRuntimeRegistrar {
    pub fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    #[hotpath::skip]
    pub async fn build_production(
        &self,
        project_root: &Path,
        input: AdvisoryRuntimeOpenV1,
        production: AdvisoryProductionOpenV1,
        lsp_session_factory: Arc<DaemonLspSessionFactory>,
    ) -> Result<Arc<AdvisoryProductionStartupRegistrationV1>, DaemonAdvisoryRuntimeRegistrationError>
    {
        let project_id = input.resolved_scope.project_id.clone();
        let feedback_registered = self
            .service
            .project_runtimes
            .read::<RegisteredFeedbackRuntime, _, _>(project_root, |runtime| {
                runtime.project_id == project_id
            })
            .await
            .unwrap_or(false);
        if !feedback_registered {
            return Err(DaemonAdvisoryRuntimeRegistrationError::MissingFeedbackRuntime);
        }
        let authorities = open_advisory_production_authorities(production)?;
        let (providers, hook_delivery_port) = authorities.into_registrar_parts();
        Ok(Arc::new(register_advisory_daemon_startup(
            input,
            providers,
            lsp_session_factory,
            hook_delivery_port,
        )?))
    }

    #[hotpath::skip]
    pub async fn publish(
        &self,
        project_root: &Path,
        registration: Arc<dyn Any + Send + Sync>,
        hook_orchestrator: Arc<BoundedHookOrchestratorV1>,
        advisory_cycle: DaemonAdvisoryCycleInvocationOwner,
        feedback_input: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Result<(), DaemonAdvisoryRuntimeRegistrationError> {
        self.service
            .project_runtimes
            .publish_advisory_atomically(
                project_root,
                super::super::project_runtime::RegisteredAdvisoryRuntimeV1::new(
                    registration,
                    hook_orchestrator,
                ),
                advisory_cycle,
                feedback_input,
            )
            .await
            .map_err(Into::into)
    }

    /// Registers the daemon-owned Delivery read authority as its own
    /// project-open component. Each project open recomputes the same
    /// daemon-owned authority for this root, so the newest open's observation
    /// replaces a displaced incumbent instead of wedging a stale gate.
    #[hotpath::skip]
    pub async fn publish_delivery_read(
        &self,
        project_root: &Path,
        authority: super::super::project_runtime::RegisteredDeliveryReadAuthorityV1,
    ) -> Result<(), DaemonAdvisoryRuntimeRegistrationError> {
        self.service
            .project_runtimes
            .publish(project_root.to_path_buf(), authority)
            .await
            .map_err(Into::into)
    }
}

impl tracedecay_dashboard_api::feedback_api::FeedbackStatusRuntime
    for DaemonFeedbackRuntimeRegistrar
{
    fn read_feedback_status(
        &self,
        project_root: PathBuf,
    ) -> tracedecay_dashboard_api::feedback_api::FeedbackStatusReadFuture {
        let registrar = self.clone();
        Box::pin(async move {
            let store = registrar.doctor_read_store(&project_root).await.ok_or(
                ApplicationContractError::Inconsistent {
                    field: "feedback status runtime",
                },
            )?;
            store.observation_read_model().await.map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "feedback status read model",
                }
            })
        })
    }
}

#[derive(Clone)]
pub struct DaemonConfigurationRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonConfigurationRuntimeRegistrar {
    pub fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    /// Verify daemon bootstrap mounted the profile-scoped worker plan before
    /// any project mode can schedule index, semantic, or session work. The
    /// profile `ProfileSessions` configuration is the sole installer; project
    /// registration must never invent or replace process CPU authority.
    pub fn ensure_worker_plan(&self) -> Result<(), TraceDecayError> {
        tracedecay_code_index::parallelism::installed_worker_status()
            .map(|_| ())
            .ok_or_else(|| TraceDecayError::Config {
                message: "profile code-index worker plan was not installed during daemon bootstrap"
                    .to_owned(),
            })
    }

    /// Commit the daemon-wide worker selection through the exact retained
    /// profile store, using the project registration only as the canonical
    /// user-profile mutation grant authority. The project configuration store
    /// is never used as a persistence sink for this setting.
    #[allow(clippy::too_many_arguments)]
    #[hotpath::skip]
    pub async fn commit_profile_code_index_worker_selection(
        &self,
        project_root: &Path,
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_id: &UserProfileId,
        request_id: &str,
        selection: tracedecay_domain::configuration::CodeIndexWorkerSelectionV1,
        expected_revision: ConfigurationRevisionId,
        idempotency_key: ConfigurationIdempotencyKey,
    ) -> Result<tracedecay_global_db::configuration::ProfileCodeIndexWorkerCommitV1, TraceDecayError>
    {
        let registered = self
            .service
            .project_runtimes
            .read::<RegisteredConfigurationRuntime, _, _>(project_root, Clone::clone)
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "profile worker configuration requires a registered project configuration grant"
                        .to_owned(),
            })?;
        if registered.project_identity.profile_id() != profile_id
            || !registered
                .project_identity
                .matches_project_root(project_root)
        {
            return Err(TraceDecayError::Config {
                message:
                    "profile worker configuration identity does not match the registered project"
                        .to_owned(),
            });
        }
        let mutation = tracedecay_configuration::profile_code_index_worker_mutation(
            database.as_ref(),
            profile_id,
            selection,
        )
        .map_err(tracedecay_configuration::map_profile_worker_configuration_error)?;
        let observed_at = current_micros();
        let authority = registered
            .grants
            .issue_direct(
                request_id,
                idempotency_key,
                &mutation,
                expected_revision.clone(),
                registered.grants.expires_at,
                observed_at,
            )
            .map_err(|_| TraceDecayError::Config {
                message: "profile worker configuration grant was refused".to_owned(),
            })?;
        tracedecay_configuration::commit_profile_code_index_worker_selection(
            database.as_ref(),
            profile_id,
            &authority,
            selection,
            &expected_revision,
        )
        .await
        .map_err(tracedecay_configuration::map_profile_worker_configuration_error)
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::skip]
    pub async fn register(
        &self,
        project_root: PathBuf,
        runtime: Arc<ProjectConfigurationRuntime>,
        scope: ResolvedScope,
        profile_id: UserProfileId,
        actor: ActorId,
        expires_at: UtcMicros,
        membership_digest: Option<ManifestDigest>,
        policy_manifest_digest: ManifestDigest,
    ) -> Result<(), TraceDecayError> {
        self.ensure_worker_plan()?;
        if self
            .service
            .project_runtimes
            .holds::<RegisteredConfigurationRuntime>(&project_root)
            .await
        {
            return Ok(());
        }
        let policy_digest = AccessPolicyDigest::new(policy_manifest_digest.as_str().to_owned())
            .map_err(|error| TraceDecayError::Config {
                message: format!("configuration policy authority is invalid: {error}"),
            })?;
        let evidence = ScopeRevalidationEvidenceV1 {
            resolved_scope_digest: scope.scope_digest.clone(),
            membership_digest,
            authorization_policy_digest: policy_digest.clone(),
            policy_epoch: 1,
        };
        let current =
            runtime
                .client()
                .current()
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("configuration layer authority unavailable: {error}"),
                })?;
        let direct_layers = mounted_configuration_layers(
            &runtime.configuration_target().project_id,
            &profile_id,
            &current.snapshot,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("configuration layer authority invalid: {error:?}"),
        })?;
        let grants = DaemonConfigurationGrantAuthority {
            actor: actor.clone(),
            policy_epoch: 1,
            policy_digest,
            expires_at,
            direct_layers: Arc::new(direct_layers),
            grants: Arc::new(RwLock::new(BTreeMap::new())),
        };
        let current = runtime.client().current().await.map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "configuration runtime activation could not read the current revision: {error}"
                ),
            }
        })?;
        runtime
            .record_runtime_activation(Some(current.revision_id), None, current_micros())
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("configuration runtime activation could not be recorded: {error}"),
            })?;
        runtime.install_authorities(
            Arc::new(DaemonConfigurationScopeResolution { actor, evidence }),
            Arc::new(PolicyBackedConfigurationMutationAuthorization::new(
                grants.clone(),
            )),
        )?;
        let project_identity = InvocationProjectRuntimeIdentityV1::new(
            profile_id,
            scope.project_id.clone(),
            project_root.clone(),
        );
        self.service
            .project_runtimes
            .publish(
                project_root,
                RegisteredConfigurationRuntime {
                    runtime,
                    scope,
                    project_identity,
                    actor: grants.actor.clone(),
                    grants,
                    semantic_operation: Arc::new(OnceLock::new()),
                    semantic_activation_committed: Arc::new(Notify::new()),
                    semantic_evaluation_workers: Arc::new(
                        tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationWorkerOwnerV1::with_scheduler_admission(
                            self.service
                                .code_index_schedulers
                                .semantic_evaluation_admission(),
                        ),
                    ),
                },
            )
            .await?;
        Ok(())
    }

    #[hotpath::skip]
    pub async fn install_semantic_operation(
        &self,
        project_root: &Path,
        operation: Arc<ProductionSemanticConfigurationOperationV1>,
    ) -> Result<(), TraceDecayError> {
        self.service
            .project_runtimes
            .read::<RegisteredConfigurationRuntime, _, _>(project_root, |registered| {
                registered
                    .semantic_operation
                    .set(operation)
                    .map_err(|_| TraceDecayError::Config {
                        message: "semantic configuration operation is already installed".to_owned(),
                    })
            })
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "semantic configuration operation requires a registered configuration runtime"
                        .to_owned(),
            })?
    }

    #[hotpath::skip]
    pub async fn install_semantic_activation_reconciler(
        &self,
        project_root: &Path,
        coordinator: Arc<
            tracedecay_usecases::semantic_runtime::ProductionSemanticActivationCoordinatorV1,
        >,
        lifecycle_events: tokio::sync::watch::Receiver<
            tracedecay_semantic_contracts::SemanticLifecycleVerifiedReadyEventV1,
        >,
    ) -> Result<(), TraceDecayError> {
        let committed_activation_wake = self
            .service
            .project_runtimes
            .read::<RegisteredConfigurationRuntime, _, _>(project_root, |registered| {
                Arc::clone(&registered.semantic_activation_committed)
            })
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "semantic activation reconciler requires a registered configuration runtime"
                        .to_owned(),
            })?;
        let reconciler = Arc::new(
            tracedecay_code_index_runtime::semantic_activation_reconciler::DaemonSemanticActivationReconcilerV1::spawn(
                coordinator,
                lifecycle_events,
                committed_activation_wake,
            ),
        );
        self.service
            .project_runtimes
            .register(project_root.to_path_buf(), reconciler)
            .await
            .map_err(|_| TraceDecayError::Config {
                message: "semantic activation reconciler is already registered".to_owned(),
            })
    }
}

#[derive(Clone)]
pub struct DaemonWorkRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonWorkRuntimeRegistrar {
    pub fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    #[hotpath::skip]
    pub async fn register(
        &self,
        project_root: PathBuf,
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        authority: WorkAuthority,
        actor: ActorId,
        grant: CapabilityGrantSnapshot,
        policy_digest: ManifestDigest,
        configuration_digest: ManifestDigest,
        work_topology_policy: tracedecay_domain::configuration::WorkTopologyPolicyV1,
        proposal_routing: DaemonWorkProposalRoutingAuthorityV1,
        evidence_retrieval: impl WorkEvidenceRetrievalPortV1 + 'static,
    ) -> Result<(), TraceDecayError> {
        let evidence_retrieval = evidence_retrieval.clone_arc();
        if authority.project_id() != &grant.scope.project_id
            || authority.repository_id() != &grant.scope.repository_id
            || authority.worktree_id() != &grant.scope.worktree_id
            || authority.actor_id() != &actor
            || authority.policy_digest() != &grant.digest
            || !proposal_routing.matches_scope(&grant.scope)
            || proposal_routing.configuration_digest() != &configuration_digest
        {
            return Err(TraceDecayError::Config {
                message: "Workflow authority does not match its registered grant".to_owned(),
            });
        }
        let authority_digest =
            canonical_sha256(&authority).map_err(|error| TraceDecayError::Config {
                message: format!("Workflow authority digest failed: {error}"),
            })?;
        let observability_producer = self
            .service
            .observability_producer(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "registered Work runtime requires the mounted observability producer"
                    .to_owned(),
            })?;
        self.service
            .project_runtimes
            .register_or_reconcile(
                project_root.clone(),
                |registered: &mut RegisteredWorkRuntime| {
                    if registered.actor == actor
                        && registered.grant.digest == grant.digest
                        && registered.grant.scope == grant.scope
                        && registered.authority_digest == authority_digest
                        && registered.policy_digest == policy_digest
                        && registered.configuration_digest == configuration_digest
                        && registered
                            .proposal_routing
                            .same_configuration_as(&proposal_routing)
                        && registered
                            .evidence_retrieval
                            .same_retrieval_authority(evidence_retrieval.as_ref())
                    {
                        // The same authority re-registering only renews its grant.
                        if registered.grant != grant {
                            let recovery = registered.workflow_fan_out_recovery.as_ref().ok_or_else(
                                || TraceDecayError::Config {
                                    message: "Workflow fan-out recovery owner is not mounted"
                                        .to_owned(),
                                },
                            )?;
                            recovery.refresh_grant(grant.clone());
                            registered.grant = grant.clone();
                        }
                        return Ok(());
                    }
                    Err(TraceDecayError::Config {
                        message:
                            "a different Work authority is already registered for this project"
                                .to_owned(),
                    })
                },
                || async {
                    let durable_write_signal =
                        super::types::WorkDurableWriteSignalV1::default();
                    let mut registered = RegisteredWorkRuntime {
                        database: database.clone(),
                        actor: actor.clone(),
                        grant: grant.clone(),
                        authority_digest: authority_digest.clone(),
                        policy_digest: policy_digest.clone(),
                        configuration_digest: configuration_digest.clone(),
                        work_topology_policy: work_topology_policy.clone(),
                        proposal_routing: proposal_routing.clone(),
                        evidence_retrieval: evidence_retrieval.clone(),
                        durable_write_signal: durable_write_signal.clone(),
                        blocked_interval_observation_recovery:
                            super::work_blocked_interval_recovery::WorkBlockedIntervalObservationRecoveryOwnerV1::mount(
                                database.clone(),
                                actor.clone(),
                                grant.clone(),
                                Arc::clone(&observability_producer),
                                durable_write_signal.subscribe(),
                            )
                            .map_err(|error| TraceDecayError::Config {
                                message: format!(
                                    "Work blocked-interval recovery owner could not mount: {error}"
                                ),
                            })?,
                        workflow_census_observation_recovery:
                            super::work::workflow_census::WorkflowFanOutCensusObservationRecoveryOwnerV1::mount(
                                database.clone(),
                                grant.scope.project_id.clone(),
                                Arc::clone(&observability_producer),
                                durable_write_signal.subscribe(),
                            )
                            .map_err(|error| TraceDecayError::Config {
                                message: format!(
                                    "Workflow census recovery owner could not mount: {error}"
                                ),
                            })?,
                        workflow_fan_out_recovery: None,
                    };
                    registered.workflow_fan_out_recovery = Some(
                        super::work::workflow_fan_out::WorkflowFanOutRecoveryOwnerV1::mount(
                            registered.clone(),
                            Arc::clone(&self.service.work_attempt_processes),
                            project_root.clone(),
                            Some(Arc::clone(&observability_producer)),
                            self.service.worktree_holder_admission.clone(),
                        )
                        .map_err(|problem| TraceDecayError::Config {
                            message: format!(
                                "Workflow fan-out recovery owner could not mount: {problem:?}"
                            ),
                        })?,
                    );
                    Ok(registered)
                },
            )
            .await?;
        Ok(())
    }

    #[hotpath::skip]
    pub async fn authority_matches(
        &self,
        project_root: &Path,
        authority: &WorkAuthority,
        actor: &ActorId,
        grant: &CapabilityGrantSnapshot,
        policy_digest: &ManifestDigest,
        configuration_digest: &ManifestDigest,
    ) -> bool {
        let Ok(authority_digest) = canonical_sha256(authority) else {
            return false;
        };
        self.service
            .project_runtimes
            .read::<RegisteredWorkRuntime, _, _>(project_root, |registered| {
                &registered.actor == actor
                    && registered.grant.digest == grant.digest
                    && registered.grant.scope == grant.scope
                    && registered.authority_digest == authority_digest
                    && &registered.policy_digest == policy_digest
                    && &registered.configuration_digest == configuration_digest
            })
            .await
            .unwrap_or(false)
    }
}

#[derive(Clone)]
pub struct DaemonRetainedRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonRetainedRuntimeRegistrar {
    pub fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    #[hotpath::skip]
    pub async fn register(
        &self,
        project_root: PathBuf,
        scope: ResolvedScope,
        actor: ActorId,
        grant: CapabilityGrantSnapshot,
        ports: Arc<tracedecay_application::retained_surfaces::RetainedSurfacePortsV1<'static>>,
    ) -> Result<(), TraceDecayError> {
        if grant.scope != scope || grant.issuer != actor {
            return Err(TraceDecayError::Config {
                message: "retained runtime grant does not match its project authority".to_owned(),
            });
        }
        self.service
            .project_runtimes
            .register_or_reconcile(
                project_root,
                |registered: &mut RegisteredRetainedRuntime| {
                    if registered.scope == scope
                        && registered.actor == actor
                        && registered.grant.digest == grant.digest
                        && Arc::ptr_eq(&registered.ports, &ports)
                    {
                        registered.grant = grant.clone();
                        Ok(())
                    } else {
                        Err(TraceDecayError::Config {
                            message: "a different retained runtime is already registered for this project"
                                .to_owned(),
                        })
                    }
                },
                || async {
                    Ok(RegisteredRetainedRuntime {
                        scope: scope.clone(),
                        actor: actor.clone(),
                        grant: grant.clone(),
                        ports: Arc::clone(&ports),
                    })
                },
            )
            .await
    }
}

/// Registers one native-integration owner per exact project/repository identity.
///
/// The owner registry lives in `tracedecay-agent-hosts`. This registrar is the
/// daemon composition entry so project-open and store administration do not
/// reach the owner map directly.
#[derive(Clone, Default)]
pub struct DaemonNativeIntegrationRuntimeRegistrar {
    registry:
        Arc<tracedecay_agent_hosts::native_integration::DaemonNativeIntegrationServiceRegistry>,
}

impl DaemonNativeIntegrationRuntimeRegistrar {
    #[hotpath::skip]
    pub async fn ensure(
        &self,
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        repository_root: PathBuf,
        project_id: ProjectId,
        repository_id: tracedecay_domain::RepositoryId,
        policy_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<
        tracedecay_agent_hosts::native_integration::DaemonNativeIntegrationOwner,
        tracedecay_application::NativeIntegrationPortError,
    > {
        self.registry
            .ensure(
                database,
                repository_root,
                project_id,
                repository_id,
                policy_digest,
                observed_at,
            )
            .await
    }

    #[hotpath::skip]
    pub async fn for_repository_root(
        &self,
        repository_root: &Path,
    ) -> Result<
        Option<tracedecay_agent_hosts::native_integration::DaemonNativeIntegrationOwner>,
        tracedecay_application::NativeIntegrationPortError,
    > {
        self.registry.for_repository_root(repository_root).await
    }

    #[hotpath::skip]
    pub async fn retire_project_database(
        &self,
        project_id: &ProjectId,
        database_path: &Path,
    ) -> Result<(), tracedecay_application::NativeIntegrationPortError> {
        self.registry
            .retire_project_database(project_id, database_path)
            .await
    }

    #[hotpath::skip]
    pub async fn shutdown(
        &self,
    ) -> Result<usize, tracedecay_application::NativeIntegrationPortError> {
        self.registry.shutdown().await
    }
}
