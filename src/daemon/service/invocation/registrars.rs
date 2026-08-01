//! Per-subsystem `*RuntimeRegistrar` newtypes and their registration error enums.

use super::*;

#[derive(Clone)]
pub(super) struct DaemonConfigurationGrantAuthority {
    actor: ActorId,
    pub(super) policy_epoch: u64,
    pub(super) policy_digest: AccessPolicyDigest,
    pub(super) expires_at: UtcMicros,
    direct_layers: Arc<BTreeMap<ManifestDigest, ConfigurationLayerIdV1>>,
    grants: Arc<RwLock<BTreeMap<ConfigurationGrantId, ConfigurationMutationGrantSnapshotV1>>>,
}

impl DaemonConfigurationGrantAuthority {
    pub(super) fn issue_direct(
        &self,
        request_id: &str,
        mutation: &DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
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
            issued_at,
        )
    }

    #[cfg(test)]
    pub(super) fn for_test(
        layers: impl IntoIterator<Item = ConfigurationLayerIdV1>,
        expires_at: UtcMicros,
    ) -> Self {
        Self {
            actor: ActorId::new("actor.configuration.test").expect("actor"),
            policy_epoch: 1,
            policy_digest: AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("policy"),
            expires_at,
            direct_layers: Arc::new(
                layers
                    .into_iter()
                    .map(|layer| {
                        let digest =
                            configuration_layer_scope_digest(&layer).expect("layer digest");
                        (digest, layer)
                    })
                    .collect(),
            ),
            grants: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub(super) fn issue(
        &self,
        request_id: &str,
        operation: ConfigurationMutationOperationV1,
        scope_digest: ManifestDigest,
        expected_revision: ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        issued_at: UtcMicros,
    ) -> Result<ConfigurationMutationAuthority, DaemonInvocationProblem> {
        if issued_at >= self.expires_at {
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
            self.expires_at,
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
            issued_at,
            self.expires_at,
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
            expires_at: self.expires_at,
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

pub(super) fn mounted_configuration_layers(
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
    ) -> crate::application::configuration::ConfigurationOperationFuture<
        'a,
        ScopeRevalidationEvidenceV1,
    > {
        let allowed = actor.actor_id == self.actor && change.validate().is_ok();
        let evidence = self.evidence.clone();
        Box::pin(async move {
            allowed
                .then_some(evidence)
                .ok_or(crate::application::configuration::ConfigurationError::TargetUnavailable)
        })
    }

    fn revalidate_plan<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        plan: &'a tracedecay_domain::configuration::ProtectedChangePlan,
    ) -> crate::application::configuration::ConfigurationOperationFuture<
        'a,
        ScopeRevalidationEvidenceV1,
    > {
        let allowed = actor.actor_id == self.actor && plan.validate().is_ok();
        let evidence = self.evidence.clone();
        Box::pin(async move {
            allowed
                .then_some(evidence)
                .ok_or(crate::application::configuration::ConfigurationError::TargetUnavailable)
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonFeedbackRuntimeRegistrationError {
    #[error("a PR12 feedback runtime is already mounted for this project database")]
    AlreadyRegistered,
    #[error("the daemon project runtime registry is closed")]
    RegistryClosed,
    #[error("the PR12 feedback runtime must be mounted before its cycle")]
    MissingRuntime,
    #[error("the PR12 feedback runtime could not be opened")]
    Runtime(#[from] Pr12FeedbackRuntimeError),
    #[error("the PR12 feedback cycle runtime could not be opened")]
    Cycle(#[from] Pr12FeedbackCycleRuntimeError),
    #[error("the PR11 policy evaluator composition is invalid")]
    Policy(#[from] ApplicationContractError),
}

impl From<ProjectRuntimeAlreadyRegistered> for DaemonFeedbackRuntimeRegistrationError {
    fn from(_: ProjectRuntimeAlreadyRegistered) -> Self {
        Self::AlreadyRegistered
    }
}

impl From<ProjectRuntimeRegistryError> for DaemonFeedbackRuntimeRegistrationError {
    fn from(error: ProjectRuntimeRegistryError) -> Self {
        match error {
            ProjectRuntimeRegistryError::AlreadyRegistered => Self::AlreadyRegistered,
            ProjectRuntimeRegistryError::Closed => Self::RegistryClosed,
        }
    }
}

impl From<FeedbackCyclePublicationError> for DaemonFeedbackRuntimeRegistrationError {
    fn from(error: FeedbackCyclePublicationError) -> Self {
        match error {
            FeedbackCyclePublicationError::Registry(error) => error.into(),
            FeedbackCyclePublicationError::RouterUnavailable => Self::MissingRuntime,
        }
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
            },
        }
    }
}

#[cfg(test)]
pub(super) struct DaemonFeedbackPublicationTestGate {
    publication_ready: StdMutex<Option<tokio::sync::oneshot::Sender<()>>>,
    continue_publication: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[cfg(test)]
impl DaemonFeedbackPublicationTestGate {
    async fn wait(&self) {
        self.publication_ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("publication-ready sender")
            .send(())
            .expect("publication-ready receiver");
        let continue_publication = self
            .continue_publication
            .lock()
            .await
            .take()
            .expect("continue-publication receiver");
        continue_publication
            .await
            .expect("continue-publication sender");
    }
}

#[derive(Clone)]
pub(crate) struct DaemonFeedbackRuntimeRegistrar {
    service: DaemonInvocationService,
    #[cfg(test)]
    pub(super) producer_constructions: Arc<AtomicUsize>,
    #[cfg(test)]
    publication_gate: Option<Arc<DaemonFeedbackPublicationTestGate>>,
}

impl DaemonFeedbackRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
            #[cfg(test)]
            producer_constructions: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            publication_gate: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_publication_gate(
        mut self,
        gate: Arc<DaemonFeedbackPublicationTestGate>,
    ) -> Self {
        self.publication_gate = Some(gate);
        self
    }

    /// Resolve the read store from the feedback runtime mounted for this exact
    /// project root. Doctor receives no provider runtime or write authority.
    pub(crate) async fn doctor_read_store(
        &self,
        project_root: &Path,
    ) -> Option<ProjectFeedbackStore> {
        self.service
            .feedback_runtime(Some(project_root))
            .await
            .map(|runtime| runtime.publication_store())
    }

    /// Registers feedback readers from the authoritative admission result.
    pub(crate) async fn open_and_register(
        &self,
        database: Database,
        project_root: PathBuf,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
        configuration: Arc<ProjectConfigurationRuntime>,
    ) -> Result<ProjectFeedbackStore, DaemonFeedbackRuntimeRegistrationError> {
        let runtime_root = project_root.clone();
        #[cfg(test)]
        let producer_constructions = Arc::clone(&self.producer_constructions);
        #[cfg(test)]
        let publication_gate = self.publication_gate.clone();
        self.service
            .project_runtimes
            .publish_feedback_atomically(project_root, move |mut publication| async move {
                let project_id = scope.project_id.clone();
                #[cfg(test)]
                producer_constructions.fetch_add(1, Ordering::SeqCst);
                let runtime = Arc::new(
                    open_pr12_feedback_runtime(
                        database,
                        runtime_root.clone(),
                        scope.clone(),
                        access,
                    )
                    .await?,
                );
                let publications = runtime.publication_store();
                let unavailable_cycle = Arc::new(UnavailableFeedbackCycleRuntimeV1::new(
                    project_id.clone(),
                    runtime.source_observation_port(),
                ));
                publication.stage(RegisteredCallableCodeRuntime {
                    authorization: DaemonCallableCodeAuthorizationSource::production(
                        runtime_root,
                        scope.clone(),
                        configuration,
                    ),
                    scope,
                })?;
                publication.stage(RegisteredFeedbackRuntime {
                    project_id,
                    runtime,
                })?;
                publication.stage(Arc::new(SwitchableFeedbackCycleRuntimeV1::new(
                    unavailable_cycle,
                )))?;
                #[cfg(test)]
                if let Some(gate) = publication_gate {
                    gate.wait().await;
                }
                Ok((publication, publications))
            })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_cycle_and_register(
        &self,
        project_root: PathBuf,
        database: Database,
        runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
        policy_context: PolicyEvaluationContextV1,
        evidence_horizon: PolicyEvidenceHorizonV1,
        evaluated_at: UtcMicros,
        provider_candidates: Vec<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1)>,
        graph: Arc<TraceDecay>,
        affected_tests: Arc<dyn AffectedTestsRetrievalPort + Send + Sync>,
        operation: ApplicationOperation,
        graph_operation: ApplicationOperation,
        tests_operation: ApplicationOperation,
        lsp_input: Pr12FeedbackCycleLspInput,
        proximity: Arc<dyn ProductionFeedbackCycleProximityPortV1>,
    ) -> Result<Arc<Pr12FeedbackCycleRuntime>, DaemonFeedbackRuntimeRegistrationError> {
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
        // The request context is validated against its grant's scope before it
        // reaches here (`RequestContext::validate` rejects a scope that differs
        // from the grant's), so this route really is scope-matched. Live
        // correlation only reads, so it requires the Read effect class.
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
                AnalyzerAdmittedDiagnosticProviderV1::evaluate_current_plan20_snapshot(
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
        let runtime = open_pr12_feedback_cycle_runtime(
            database,
            feedback,
            runtime_state,
            correlation_policy,
            provider_admissions,
            graph,
            affected_tests,
            observations,
            operation,
            graph_operation,
            tests_operation,
            lsp_input,
            Some(Arc::new(self.service.code_index_schedulers.clone())),
        )?;
        let production_input = production_proximity_feedback_cycle_input(
            runtime.clone(),
            production_lsp_input,
            proximity,
        );
        self.service
            .project_runtimes
            .publish_feedback_cycle_atomically(project_root, runtime.clone(), production_input)
            .await
            .map_err(DaemonFeedbackRuntimeRegistrationError::from)?;
        Ok(runtime)
    }

    pub(crate) async fn install_advisory_cycle_input(
        &self,
        project_root: &Path,
        input: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Result<(), DaemonFeedbackRuntimeRegistrationError> {
        self.service
            .project_runtimes
            .replace_feedback_cycle_input_atomically(project_root, input)
            .await
            .map_err(DaemonFeedbackRuntimeRegistrationError::from)
    }
}

impl crate::dashboard::feedback_api::FeedbackStatusRuntime for DaemonFeedbackRuntimeRegistrar {
    fn read_feedback_status(
        &self,
        project_root: PathBuf,
    ) -> crate::dashboard::feedback_api::FeedbackStatusReadFuture {
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
pub(crate) struct DaemonConfigurationRuntimeRegistrar {
    service: DaemonInvocationService,
}

pub(crate) enum DoctorConfigurationOutcomeV1 {
    Preview {
        preview_id: PreviewId,
        execution: OperationReceipt,
    },
    Effect {
        execution: OperationReceipt,
        receipt: Box<EffectReceipt>,
    },
    Denied,
    Unavailable,
}

impl DaemonConfigurationRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn doctor_owner_mounted(&self, project_root: &Path) -> bool {
        self.service
            .configuration_runtime(Some(project_root))
            .await
            .is_some()
    }

    pub(crate) async fn doctor_execute(
        &self,
        project_root: &Path,
        request_id: &RequestId,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: ConfigurationSurfaceRequest,
    ) -> DoctorConfigurationOutcomeV1 {
        let Some(registered) = self.service.configuration_runtime(Some(project_root)).await else {
            return DoctorConfigurationOutcomeV1::Unavailable;
        };
        let observed_at = current_micros();
        let deadline = match Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))) {
            Ok(deadline) => deadline,
            Err(_) => return DoctorConfigurationOutcomeV1::Unavailable,
        };
        let cancellation = match CancellationContext::active(format!(
            "cancel.doctor-remediation.{}",
            request_id.as_str()
        )) {
            Ok(cancellation) => cancellation,
            Err(_) => return DoctorConfigurationOutcomeV1::Unavailable,
        };
        let response = execute_configuration(
            request_id.as_str().to_owned(),
            Some(registered),
            surface_operation,
            request,
            observed_at,
            deadline,
            cancellation,
        )
        .await;
        match response.outcome {
            DaemonInvocationOutcome::Configuration {
                outcome: ApplicationOutcome::Preview(preview),
                ..
            } => DoctorConfigurationOutcomeV1::Preview {
                preview_id: preview.preview_id,
                execution: preview.execution,
            },
            DaemonInvocationOutcome::Configuration {
                outcome: ApplicationOutcome::Effect(effect),
                ..
            } => DoctorConfigurationOutcomeV1::Effect {
                execution: effect.execution,
                receipt: Box::new(effect.receipt),
            },
            DaemonInvocationOutcome::ApplicationProblem {
                problem: ApplicationProblem::NotFoundOrNotAuthorized { .. },
            } => DoctorConfigurationOutcomeV1::Denied,
            _ => DoctorConfigurationOutcomeV1::Unavailable,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn register(
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
        self.service
            .project_runtimes
            .publish(
                project_root,
                RegisteredConfigurationRuntime {
                    runtime,
                    scope,
                    actor: grants.actor.clone(),
                    grants,
                    semantic_operation: Arc::new(OnceLock::new()),
                },
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn install_semantic_operation(
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
                message: "semantic configuration operation requires a registered Plan 20 runtime"
                    .to_owned(),
            })?
    }
}

#[derive(Clone)]
pub(crate) struct DaemonWorkRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonWorkRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        database: Arc<crate::global_db::RegisteredGlobalDb>,
        authority: WorkAuthority,
        actor: ActorId,
        grant: CapabilityGrantSnapshot,
        policy_digest: ManifestDigest,
        configuration_digest: ManifestDigest,
        config: crate::sessions::codex_app_server::CodexAppServerSummaryConfig,
    ) -> Result<(), TraceDecayError> {
        if authority.project_id() != &grant.scope.project_id
            || authority.repository_id() != &grant.scope.repository_id
            || authority.worktree_id() != &grant.scope.worktree_id
            || authority.actor_id() != &actor
            || authority.policy_digest() != &grant.digest
        {
            return Err(TraceDecayError::Config {
                message: "Work runtime authority does not match its registered grant".to_owned(),
            });
        }
        let authority_digest =
            canonical_sha256(&authority).map_err(|error| TraceDecayError::Config {
                message: format!("Work runtime authority digest failed: {error}"),
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
                    {
                        // The same authority re-registering only renews its grant.
                        registered.grant = grant.clone();
                        return Ok(());
                    }
                    Err(TraceDecayError::Config {
                        message:
                            "a different Work authority is already registered for this project"
                                .to_owned(),
                    })
                },
                || {
                    // Opening the provider runtime is deferred until the slot is
                    // known to be free so a refused registration never starts one.
                    let runtime = DaemonWorkRuntimeV1::new(
                        authority,
                        database.work_storage()?,
                        config,
                        configuration_digest.clone(),
                        Arc::clone(&database),
                        project_root.clone(),
                    );
                    Ok(RegisteredWorkRuntime {
                        database,
                        runtime: Arc::new(runtime),
                        actor: actor.clone(),
                        grant: grant.clone(),
                        authority_digest: authority_digest.clone(),
                        policy_digest: policy_digest.clone(),
                        configuration_digest: configuration_digest.clone(),
                    })
                },
            )
            .await
    }

    pub(crate) async fn authority_matches(
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
pub(crate) struct DaemonLspOwnerRegistrar {
    service: DaemonInvocationService,
}

impl DaemonLspOwnerRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register_lsp_owner(
        &self,
        project_root: PathBuf,
        owner: DaemonLspInvocationOwner,
    ) -> Result<(), ProjectRuntimeRegistryError> {
        self.service.install_lsp_owner(project_root, owner).await
    }

    #[cfg(test)]
    pub(crate) async fn register_factory(
        &self,
        project_root: PathBuf,
        factory: Arc<DaemonLspSessionFactory>,
    ) -> Result<(), ProjectRuntimeRegistryError> {
        self.register_lsp_owner(project_root, DaemonLspInvocationOwner::new(factory))
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build_and_register(
        &self,
        project_root: PathBuf,
        scope_grant: CapabilityGrantSnapshot,
        registered_database: Arc<crate::global_db::RegisteredGlobalDb>,
        database: Database,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
        runtime: tokio::runtime::Handle,
        diagnostic_broker: Arc<Mutex<DiagnosticBroker>>,
        languages: &[String],
        root_uri: String,
        timeouts: LspRefreshTimeouts,
        diagnostics_quiet_window: Duration,
        gateway_capabilities: GatewayCapabilities,
    ) -> Result<Arc<DaemonLspSessionFactory>, TraceDecayError> {
        let feedback_runtime = self
            .service
            .feedback_runtime(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "PR12 feedback runtime is not registered for the project".to_owned(),
            })?;
        let feedback_cycle_input = self
            .service
            .feedback_cycle_input(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "production feedback cycle input is not registered for the project"
                    .to_owned(),
            })?;
        let semantics = production_semantic_authorities(
            runtime.clone(),
            diagnostic_broker.clone(),
            database.clone(),
            languages,
            project_root.clone(),
            root_uri,
            timeouts,
        )
        .await?;
        let upstream_capabilities = UpstreamCapabilities {
            supports_diagnostics: semantics.analyzer_available,
            semantic: semantics.semantic_capabilities.clone(),
        };
        let factory = Arc::new(
            lsp_session_factory(
                runtime,
                feedback_runtime,
                database,
                code_index,
                move |_| Arc::clone(&feedback_cycle_input),
                semantics.semantics,
                diagnostic_broker,
                diagnostics_quiet_window,
                semantics.cancellation,
                gateway_capabilities,
                upstream_capabilities,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not construct LSP session factory: {error:?}"),
            })?,
        );
        let scope_set_storage = registered_database.authorized_scope_set_storage()?;
        self.register_lsp_owner(
            project_root,
            DaemonLspInvocationOwner::authorized(factory.clone(), scope_grant, scope_set_storage),
        )
        .await?;
        Ok(factory)
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonAdvisoryRuntimeRegistrationError {
    #[error("a PR13 advisory runtime is already mounted for this project")]
    AlreadyRegistered,
    #[error("the daemon project runtime registry is closed")]
    RegistryClosed,
    #[error("the shared PR12 feedback readers must be registered before PR13")]
    MissingFeedbackRuntime,
    #[error("the PR13 Hook orchestration registry is unavailable")]
    HookOrchestrationUnavailable,
    #[error("the PR13 production authorities could not be opened")]
    Production(#[from] Pr13AdvisoryProductionOpenErrorV1),
    #[error(transparent)]
    Startup(#[from] Pr13AdvisoryDaemonStartupErrorV1),
}

impl From<ProjectRuntimeRegistryError> for DaemonAdvisoryRuntimeRegistrationError {
    fn from(error: ProjectRuntimeRegistryError) -> Self {
        match error {
            ProjectRuntimeRegistryError::AlreadyRegistered => Self::AlreadyRegistered,
            ProjectRuntimeRegistryError::Closed => Self::RegistryClosed,
        }
    }
}

#[derive(Clone)]
pub(crate) struct DaemonAdvisoryRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonAdvisoryRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register<GR, GA, CS, CE, PE, PC>(
        &self,
        project_root: PathBuf,
        input: Pr13AdvisoryRuntimeOpenV1,
        providers: Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
        lsp_session_factory: Arc<DaemonLspSessionFactory>,
        hook_delivery_port: Arc<
            dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync,
        >,
    ) -> Result<
        Arc<Pr13AdvisoryDaemonStartupRegistrationV1<GR, GA, CS, CE, PE, PC>>,
        DaemonAdvisoryRuntimeRegistrationError,
    >
    where
        GR: GitHubCurrentBranchRemapper + Send + Sync + 'static,
        GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Send + Sync + 'static,
        CS: CiReadOnlyProviderArchiveV1 + Send + Sync + 'static,
        CE: CiExactEvidenceAuthorityV1<CS::Record> + Send + Sync + 'static,
        PE: CanonicalProximityEvidenceAuthorityV1 + Send + Sync + 'static,
        PC: ConfigurationControlStore + Clone + Send + Sync + 'static,
    {
        let project_id = input.resolved_scope.project_id.clone();
        let feedback_registered = self
            .service
            .project_runtimes
            .read::<RegisteredFeedbackRuntime, _, _>(&project_root, |runtime| {
                runtime.project_id == project_id
            })
            .await
            .unwrap_or(false);
        if !feedback_registered {
            return Err(DaemonAdvisoryRuntimeRegistrationError::HookOrchestrationUnavailable);
        }
        if self
            .service
            .project_runtimes
            .holds::<Arc<dyn Any + Send + Sync>>(&project_root)
            .await
        {
            return Err(DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered);
        }
        let registration = Arc::new(register_pr13_advisory_daemon_startup(
            input,
            providers,
            lsp_session_factory.clone(),
            hook_delivery_port,
        )?);
        let registered_root = project_root.clone();
        let published: Arc<dyn Any + Send + Sync> = registration.clone();
        self.service
            .project_runtimes
            .register(project_root, published)
            .await
            .map_err(DaemonAdvisoryRuntimeRegistrationError::from)?;
        self.service
            .install_lsp_owner(
                registered_root,
                DaemonLspInvocationOwner::new(lsp_session_factory),
            )
            .await
            .map_err(DaemonAdvisoryRuntimeRegistrationError::from)?;
        Ok(registration)
    }

    pub(crate) async fn register_production(
        &self,
        project_root: PathBuf,
        input: Pr13AdvisoryRuntimeOpenV1,
        production: Pr13AdvisoryProductionOpenV1,
        lsp_session_factory: Arc<DaemonLspSessionFactory>,
    ) -> Result<
        Arc<Pr13AdvisoryProductionStartupRegistrationV1>,
        DaemonAdvisoryRuntimeRegistrationError,
    > {
        let authorities = open_pr13_advisory_production_authorities(production)?;
        let (providers, hook_delivery_port) = authorities.into_registrar_parts();
        self.register(
            project_root,
            input,
            providers,
            lsp_session_factory,
            hook_delivery_port,
        )
        .await
    }

    pub(crate) async fn register_hook_orchestrator(
        &self,
        project_root: PathBuf,
        project_id: [u8; 16],
        worktree_id: [u8; 16],
        runtime: Arc<dyn Pr13HookOrchestrationPortV1>,
    ) -> Result<(), DaemonAdvisoryRuntimeRegistrationError> {
        if project_id == [0; 16]
            || worktree_id == [0; 16]
            || !self
                .service
                .project_runtimes
                .holds::<Arc<dyn Any + Send + Sync>>(&project_root)
                .await
        {
            return Err(DaemonAdvisoryRuntimeRegistrationError::MissingFeedbackRuntime);
        }
        self.service
            .project_runtimes
            .register(project_root.clone(), Arc::clone(&runtime))
            .await
            .map_err(DaemonAdvisoryRuntimeRegistrationError::from)?;
        let runtime_weak: Weak<dyn Pr13HookOrchestrationPortV1> = Arc::downgrade(&runtime);
        let registered = match pr13_hook_orchestration_registry().lock() {
            Ok(mut registry) => {
                registry.retain(|_, runtime| runtime.strong_count() > 0);
                let key = (project_id, worktree_id);
                if registry
                    .get(&key)
                    .and_then(Weak::upgrade)
                    .is_some_and(|existing| !Arc::ptr_eq(&existing, &runtime))
                {
                    false
                } else {
                    registry.insert(key, runtime_weak);
                    true
                }
            }
            Err(_) => false,
        };
        if registered {
            Ok(())
        } else {
            self.service
                .project_runtimes
                .withdraw::<Arc<dyn Pr13HookOrchestrationPortV1>>(&project_root)
                .await;
            Err(DaemonAdvisoryRuntimeRegistrationError::HookOrchestrationUnavailable)
        }
    }
}
