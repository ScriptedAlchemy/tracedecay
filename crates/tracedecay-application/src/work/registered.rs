use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::shard_runtime::{
    VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1,
};

#[derive(Clone)]
pub struct RegisteredWorkflowTopologyV1 {
    source: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    runtime: VerifiedGraphRuntimeWeakProxyV1,
}

impl RegisteredWorkflowTopologyV1 {
    pub fn verified_snapshot(
        &self,
        definition_id: &tracedecay_domain::WorkflowDefinitionId,
        definition_version: u64,
        cancelled: Arc<AtomicBool>,
    ) -> Result<
        crate::work::workflow_topology::WorkflowTopologyStore,
        crate::work::workflow_topology::WorkflowTopologyError,
    > {
        let definition = self
            .source
            .load_definition_source(definition_id, definition_version)
            .map_err(|error| {
                crate::work::workflow_topology::WorkflowTopologyError::Unavailable(format!(
                    "{error:?}"
                ))
            })?
            .ok_or_else(|| {
                crate::work::workflow_topology::WorkflowTopologyError::Unavailable(
                    "workflow definition source is missing".to_owned(),
                )
            })?;
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(tracedecay_graph_db::GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        crate::work::workflow_topology::WorkflowTopologyStore::publish_from_definition(
            &definition,
            &check,
            |manifest, key| {
                self.runtime
                    .publish_verified_manifest(manifest, key, Arc::clone(&cancelled))
            },
        )
    }
}

/// Core Work command and projection services over the registered exact-SQL
/// channel.
pub struct RegisteredWorkApplicationServicesV1 {
    attempts: tracedecay_contracts::WorkAttemptService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    attempt_effects: tracedecay_contracts::WorkAttemptEffectServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    run_control: tracedecay_contracts::WorkRunControlService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    placement: tracedecay_contracts::WorkPlacementService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    artifact_hydration: tracedecay_contracts::WorkArtifactHydrationService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    duplicate_adjudications: tracedecay_contracts::WorkDuplicateAdjudicationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    storage: tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
}

/// The Work product graph authority: its verified reads and its journaled
/// mutations, both over the same registered store.
///
/// This is a second Work authority, not a view of the first. The task services
/// above are scoped by `WorkAuthority`; this one is scoped by the registered
/// profile owner, which is also where its owner identity comes from, the
/// store's own binding, never a value a request supplied.
pub struct RegisteredWorkProductServicesV1 {
    reads: tracedecay_contracts::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    mutations: tracedecay_contracts::WorkProductMutationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    attempts: tracedecay_contracts::WorkProductAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    synthesis: tracedecay_contracts::WorkProductSynthesisAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    retry: tracedecay_contracts::WorkProductRetryServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_contracts::RuntimeWorkRetryEvidenceV1,
    >,
}

impl RegisteredWorkProductServicesV1 {
    /// Attaches the Work product graph authority over the registered exact-SQL
    /// handle.
    ///
    /// The catalog binding is supplied by the caller rather than minted here,
    /// because a service composed against a capability the catalog does not
    /// advertise could never authorize a request: it would look wired and
    /// answer nothing. Whichever adapter mounts a Work product operation
    /// passes that operation's own capability and use-case ids.
    ///
    /// The owner identity is NOT a parameter. It is resolved from the store's
    /// own registered binding, so no caller can ask for another profile's Work
    /// product by naming it.
    pub fn attach(
        db: &RegisteredGlobalDb,
        binding: tracedecay_contracts::WorkProductBindingV1,
    ) -> tracedecay_domain::errors::Result<Self> {
        let storage = db.work_storage()?;
        Ok(Self {
            reads: tracedecay_contracts::WorkProductReadServiceV1::new(
                storage.clone(),
                storage.clone(),
                binding,
            ),
            mutations: tracedecay_contracts::WorkProductMutationServiceV1::new(
                storage.clone(),
                storage.clone(),
                storage.clone(),
            ),
            attempts: tracedecay_contracts::WorkProductAttemptServiceV1::new(storage.clone()),
            synthesis: tracedecay_contracts::WorkProductSynthesisAttemptServiceV1::new(
                storage.clone(),
            ),
            retry: tracedecay_contracts::WorkProductRetryServiceV1::new(
                storage,
                tracedecay_contracts::RuntimeWorkRetryEvidenceV1,
            ),
        })
    }

    pub fn attach_with_attempt_authority(
        db: &RegisteredGlobalDb,
        binding: tracedecay_contracts::WorkProductBindingV1,
        authority: tracedecay_domain::WorkAuthority,
    ) -> tracedecay_domain::errors::Result<RegisteredAuthorizedWorkProductServicesV1> {
        let storage = db.work_storage()?;
        Ok(RegisteredAuthorizedWorkProductServicesV1 {
            reads: tracedecay_contracts::WorkProductReadServiceV1::new(
                tracedecay_rusqlite_runtime::work_product::AuthorizedWorkProductReadStorageV1::new(
                    storage.clone(),
                    authority,
                ),
                storage,
                binding,
            ),
        })
    }

    pub const fn reads(
        &self,
    ) -> &tracedecay_contracts::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.reads
    }

    pub const fn mutations(
        &self,
    ) -> &tracedecay_contracts::WorkProductMutationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.mutations
    }

    pub const fn attempts(
        &self,
    ) -> &tracedecay_contracts::WorkProductAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.attempts
    }

    pub const fn synthesis(
        &self,
    ) -> &tracedecay_contracts::WorkProductSynthesisAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.synthesis
    }

    pub const fn retry(
        &self,
    ) -> &tracedecay_contracts::WorkProductRetryServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_contracts::RuntimeWorkRetryEvidenceV1,
    > {
        &self.retry
    }
}

pub struct RegisteredAuthorizedWorkProductServicesV1 {
    reads: tracedecay_contracts::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work_product::AuthorizedWorkProductReadStorageV1,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
}

impl RegisteredAuthorizedWorkProductServicesV1 {
    pub const fn reads(
        &self,
    ) -> &tracedecay_contracts::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work_product::AuthorizedWorkProductReadStorageV1,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.reads
    }
}

impl RegisteredWorkApplicationServicesV1 {
    pub fn attach(db: &RegisteredGlobalDb) -> tracedecay_domain::errors::Result<Self> {
        let storage = db.work_storage()?;
        Ok(Self {
            attempts: tracedecay_contracts::WorkAttemptService::new(storage.clone()),
            attempt_effects: tracedecay_contracts::WorkAttemptEffectServiceV1::new(storage.clone()),
            run_control: tracedecay_contracts::WorkRunControlService::new(storage.clone()),
            placement: tracedecay_contracts::WorkPlacementService::new(storage.clone()),
            artifact_hydration: tracedecay_contracts::WorkArtifactHydrationService::new(
                storage.clone(),
            ),
            duplicate_adjudications: tracedecay_contracts::WorkDuplicateAdjudicationServiceV1::new(
                storage.clone(),
            ),
            storage,
        })
    }

    /// Pauses one run as a single writer transaction: the frontier read, the
    /// decision and the publication commit together or not at all.
    pub async fn pause_run(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::PauseWorkRunCommand,
    ) -> Result<
        tracedecay_contracts::WorkRunControlTransitionReceiptV1,
        tracedecay_contracts::ApplicationProblem,
    > {
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkRunControlService::new(storage)
                    .pause_with_receipt(&context, command.clone())
            })
            .await
            .unwrap_or_else(|_| Err(run_control_unavailable()))
    }

    /// Resumes one run as a single writer transaction.
    pub async fn resume_run(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::ResumeWorkRunCommand,
    ) -> Result<
        tracedecay_contracts::WorkRunControlTransitionReceiptV1,
        tracedecay_contracts::ApplicationProblem,
    > {
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkRunControlService::new(storage)
                    .resume_with_receipt(&context, command.clone())
            })
            .await
            .unwrap_or_else(|_| Err(run_control_unavailable()))
    }

    /// Records one duplicate adjudication as a single writer transaction: the
    /// replay check, the revision compare and the append commit together.
    /// `current_evidence` is read before the job, outside the writer.
    pub async fn adjudicate_duplicate(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_domain::WorkDuplicateAdjudicationCommandV1,
        current_evidence: tracedecay_domain::WorkDuplicateAdjudicationEvidenceV1,
    ) -> Result<
        tracedecay_contracts::WorkDuplicateAdjudicationAppendOutcomeV1,
        tracedecay_contracts::ApplicationProblem,
    > {
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkDuplicateAdjudicationServiceV1::new(storage).adjudicate(
                    &context,
                    command.clone(),
                    current_evidence.clone(),
                )
            })
            .await
            .unwrap_or_else(|_| {
                Err(
                    tracedecay_contracts::WorkDuplicateAdjudicationStorageErrorV1::Unavailable
                        .into(),
                )
            })
    }

    /// Records one leak adjudication as a single writer transaction: the
    /// command replay check, the verdict over `evidence` and the
    /// revision-compared append commit together. `evidence` is collected
    /// before the job, outside the writer.
    pub async fn adjudicate_leak<E>(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::AdjudicateWorkLeakCommandV1,
        evidence: E,
        scan_started_at: tracedecay_domain::UtcMicros,
        scan_deadline: tracedecay_domain::UtcMicros,
    ) -> Result<
        tracedecay_contracts::WorkLeakAdjudicationOutcomeV1,
        tracedecay_contracts::ApplicationProblem,
    >
    where
        E: tracedecay_contracts::WorkLeakEvidencePortV1 + Clone + 'static,
    {
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkLeakAdjudicationServiceV1::new(storage, evidence.clone())
                    .adjudicate(&context, command.clone(), scan_started_at, scan_deadline)
            })
            .await
            .unwrap_or_else(|_| {
                Err(tracedecay_contracts::WorkLeakAdjudicationStorageErrorV1::Unavailable.into())
            })
    }

    /// Admits one placement as a single writer transaction: the replay check,
    /// the exclusivity read and the publication commit together. `observe`
    /// reads the target (Git) before the job, never on the writer.
    pub async fn admit_placement(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::AdmitWorkPlacementCommand,
        observe: impl FnOnce(
            &tracedecay_domain::WorkPlacementTargetV1,
        ) -> Result<
            tracedecay_domain::WorkPlacementObservationV1,
            tracedecay_contracts::ApplicationProblem,
        >,
    ) -> Result<tracedecay_domain::WorkPlacementV1, tracedecay_contracts::ApplicationProblem> {
        let observation = observe(&command.target)?;
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkPlacementService::new(storage).admit_placement(
                    &context,
                    command.clone(),
                    |_| Ok(observation),
                )
            })
            .await
            .unwrap_or_else(|_| Err(placement_unavailable()))
    }

    /// Releases one placement. The placement is read on a reader and its
    /// target observed before the job; the job then publishes only while the
    /// placement still has the observed target and the expected authority
    /// version, so the Git observation never runs on the writer.
    pub async fn release_placement(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::ReleaseWorkPlacementCommand,
        observe: impl FnOnce(
            &tracedecay_domain::WorkPlacementTargetV1,
        ) -> Result<
            tracedecay_domain::WorkPlacementObservationV1,
            tracedecay_contracts::ApplicationProblem,
        >,
    ) -> Result<tracedecay_domain::WorkPlacementV1, tracedecay_contracts::ApplicationProblem> {
        let authority = tracedecay_contracts::work_authority(&context)?;
        let identity = tracedecay_domain::WorkPlacementIdentityV1::new(
            command.task_id.clone(),
            command.run_id.clone(),
        );
        let current = self
            .storage
            .load_placement_async(&authority, &identity)
            .await?
            .ok_or(tracedecay_contracts::WorkPlacementStorageError::NotFoundOrNotAuthorized)?;
        if current.authority_version() != command.expected_authority_version {
            return Err(tracedecay_contracts::WorkPlacementStorageError::AuthorityConflict.into());
        }
        let observation = observe(current.target())?;
        let observed_target = current.target().clone();
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkPlacementService::new(storage).release(
                    &context,
                    command.clone(),
                    |target| {
                        if target == &observed_target {
                            Ok(observation)
                        } else {
                            Err(
                                tracedecay_contracts::WorkPlacementStorageError::AuthorityConflict
                                    .into(),
                            )
                        }
                    },
                )
            })
            .await
            .unwrap_or_else(|_| Err(placement_unavailable()))
    }

    pub fn attempts(
        &self,
    ) -> &tracedecay_contracts::WorkAttemptService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.attempts
    }

    pub const fn attempt_effects(
        &self,
    ) -> &tracedecay_contracts::WorkAttemptEffectServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.attempt_effects
    }

    /// The run-level pause/resume authority.
    ///
    /// It is a separate service from [`Self::attempts`] because the aggregate
    /// it owns is separate: an attempt lease fences one attempt, while the run
    /// control fences every future reservation of the run.
    pub const fn run_control(
        &self,
    ) -> &tracedecay_contracts::WorkRunControlService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.run_control
    }

    /// The placement preflight/admit/status/release authority.
    pub const fn placement(
        &self,
    ) -> &tracedecay_contracts::WorkPlacementService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.placement
    }

    /// The artifact and evidence hydration read authority.
    pub const fn artifact_hydration(
        &self,
    ) -> &tracedecay_contracts::WorkArtifactHydrationService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.artifact_hydration
    }

    /// Explicit revisioned duplicate-effort adjudication authority.
    pub const fn duplicate_adjudications(
        &self,
    ) -> &tracedecay_contracts::WorkDuplicateAdjudicationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.duplicate_adjudications
    }
}

/// Workflow definition reads and journaled mutation authority over the
/// registered exact-SQL channel.
///
/// [`WorkflowSqliteAuthority`]: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority
pub struct RegisteredWorkflowApplicationServicesV1 {
    definitions: tracedecay_contracts::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    >,
    effects: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    topology: RegisteredWorkflowTopologyV1,
}

impl RegisteredWorkflowApplicationServicesV1 {
    pub fn attach(db: &RegisteredGlobalDb) -> tracedecay_domain::errors::Result<Self> {
        let authority = db.workflow_storage()?;
        let runtime = db.project_graph_runtime().cloned().ok_or_else(|| {
            attach_error(
                "attach registered workflow topology",
                "project graph runtime is not bound",
            )
        })?;
        Ok(Self {
            definitions: tracedecay_contracts::WorkflowDefinitionService::new(authority.clone()),
            effects: authority.clone(),
            topology: RegisteredWorkflowTopologyV1 {
                source: authority,
                runtime,
            },
        })
    }

    pub fn definitions(
        &self,
    ) -> &tracedecay_contracts::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        &self.definitions
    }

    pub fn effects(&self) -> &tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority {
        &self.effects
    }

    pub fn has_pending_effects(
        &self,
        worktree_id: &tracedecay_domain::WorktreeId,
    ) -> Result<bool, tracedecay_contracts::WorkflowEffectAuthorityErrorV1> {
        tracedecay_contracts::WorkflowEffectAuthorityPortV1::has_pending_effects(
            &self.effects,
            worktree_id,
        )
    }

    pub fn topology(&self) -> &RegisteredWorkflowTopologyV1 {
        &self.topology
    }
}

/// Attaches product intelligence to the canonical verified Work graph and
/// rooted-evidence authorities owned by this registered exact-SQL store,
/// reading only through `authority`.
pub fn authorized_work_intelligence_service(
    db: &RegisteredGlobalDb,
    binding: tracedecay_contracts::WorkProductBindingV1,
    authority: tracedecay_domain::WorkAuthority,
) -> tracedecay_domain::errors::Result<
    tracedecay_contracts::WorkIntelligenceServiceV1<
        tracedecay_rusqlite_runtime::work_product::AuthorizedWorkProductReadStorageV1,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
> {
    let storage = db.work_storage()?;
    Ok(tracedecay_contracts::WorkIntelligenceServiceV1::new(
        tracedecay_rusqlite_runtime::work_product::AuthorizedWorkProductReadStorageV1::new(
            storage.clone(),
            authority,
        ),
        storage,
        binding,
    ))
}

fn placement_unavailable() -> tracedecay_contracts::ApplicationProblem {
    tracedecay_contracts::WorkPlacementStorageError::Unavailable.into()
}

fn run_control_unavailable() -> tracedecay_contracts::ApplicationProblem {
    tracedecay_contracts::WorkRunControlStorageError::Unavailable.into()
}

fn attach_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}
