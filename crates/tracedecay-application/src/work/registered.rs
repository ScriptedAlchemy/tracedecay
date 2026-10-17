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
    storage: tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
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
            storage,
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

    /// Starts one attempt as a single writer transaction: the run's pause
    /// fence, the replay check, the lease epoch, the product link and the
    /// attempt row commit together, so a pause cannot land between the fence
    /// and the reservation. The current product graph is read before the job,
    /// on a reader; the link's graph-version compare-and-swap fences it.
    pub async fn start_attempt(
        &self,
        context: tracedecay_contracts::RequestContext,
        binding: tracedecay_contracts::WorkProductBindingV1,
        revisions: tracedecay_contracts::WorkProductRevisionPinsV1,
        topology: tracedecay_domain::WorkTopologyPolicyV1,
        command: tracedecay_contracts::StartWorkAttemptCommand,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        let prefetch_context = context.clone();
        let observed_at = command.occurred_at;
        let graph = self
            .storage
            .prefetch_graph(move |storage| {
                tracedecay_contracts::PrefetchedWorkGraphReadV1::for_attempt_admission(
                    storage,
                    &prefetch_context,
                    observed_at,
                )
            })
            .await;
        self.storage
            .transact_with_graph(graph, move |storage| {
                tracedecay_contracts::WorkRunControlService::new(storage).admit_reservation(
                    &context,
                    &command.task_id,
                    &command.run_id,
                )?;
                tracedecay_contracts::WorkProductAttemptServiceV1::new(storage)
                    .start_against_registered_topology(
                        &context,
                        &binding,
                        &revisions,
                        &topology,
                        command.clone(),
                    )
            })
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
    }

    /// Commits one product graph mutation as a single writer transaction: the
    /// revision check, the command replay check and the event append with its
    /// verified version commit together. The current graph the mutation
    /// checks its expected version against is read before the job, on a
    /// reader; the append's graph-version compare-and-swap fences it.
    pub async fn mutate(
        &self,
        context: tracedecay_contracts::RequestContext,
        binding: tracedecay_contracts::WorkProductBindingV1,
        request: tracedecay_contracts::WorkProductMutationRequestV1,
        revisions: tracedecay_contracts::WorkProductRevisionPinsV1,
    ) -> Result<
        tracedecay_contracts::WorkProductMutationReceiptV1,
        tracedecay_contracts::WorkProductApplicationErrorV1,
    > {
        let (prefetch_context, prefetch_request) = (context.clone(), request.clone());
        let graph = self
            .storage
            .prefetch_graph(move |storage| {
                tracedecay_contracts::PrefetchedWorkGraphReadV1::for_mutation(
                    storage,
                    &prefetch_context,
                    &prefetch_request,
                )
            })
            .await;
        self.storage
            .transact_with_graph(graph, move |storage| {
                tracedecay_contracts::WorkProductMutationServiceV1::new(storage, storage, storage)
                    .mutate(&context, &binding, request.clone(), &revisions)
            })
            .await
            .unwrap_or(Err(
                tracedecay_contracts::WorkProductApplicationErrorV1::EventAuthorityUnavailable,
            ))
    }

    /// Admits one synthesis attempt as a single writer transaction: the
    /// replay check, the source reads, the run's pause fence, the source
    /// payload hydration `prepare_instructions` does through the job storage
    /// and the admission commit together.
    #[allow(clippy::too_many_arguments)]
    pub async fn synthesize<F>(
        &self,
        context: tracedecay_contracts::RequestContext,
        binding: tracedecay_contracts::WorkProductBindingV1,
        revisions: tracedecay_contracts::WorkProductRevisionPinsV1,
        topology: tracedecay_domain::WorkTopologyPolicyV1,
        command: tracedecay_contracts::AdmitWorkSynthesisCommand,
        prepare_instructions: F,
    ) -> Result<
        tracedecay_contracts::WorkSynthesisAttemptV1,
        tracedecay_contracts::ApplicationProblem,
    >
    where
        F: Fn(
                &tracedecay_rusqlite_runtime::work_run_control::WorkSqliteJobStorage<'_>,
                &tracedecay_contracts::StartWorkAttemptCommand,
                &[tracedecay_domain::WorkAttemptV1],
            ) -> Result<String, tracedecay_contracts::ApplicationProblem>
            + Send
            + Sync
            + 'static,
    {
        let prefetch_context = context.clone();
        let observed_at = command.start.occurred_at;
        let graph = self
            .storage
            .prefetch_graph(move |storage| {
                tracedecay_contracts::PrefetchedWorkGraphReadV1::for_attempt_admission(
                    storage,
                    &prefetch_context,
                    observed_at,
                )
            })
            .await;
        self.storage
            .transact_with_graph(graph, move |storage| {
                tracedecay_contracts::admit_work_synthesis_against_registered_topology(
                    &tracedecay_contracts::WorkProductSynthesisAttemptServiceV1::new(storage),
                    &context,
                    &binding,
                    &revisions,
                    &topology,
                    command.clone(),
                    |start, sources| {
                        tracedecay_contracts::WorkRunControlService::new(storage)
                            .admit_reservation(&context, &start.task_id, &start.run_id)?;
                        prepare_instructions(&storage, start, sources)
                    },
                )
            })
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
    }

    /// Admits one retry as a single writer transaction: the replay check, the
    /// effect-safety read, the lease epoch, the product link, the retry
    /// receipt, the attempt row and the workflow child rebind commit together.
    /// `workflow_rebind` is read before the job; its journal append is
    /// sequence-checked inside it.
    #[allow(clippy::too_many_arguments)]
    pub async fn retry_attempt(
        &self,
        context: tracedecay_contracts::RequestContext,
        binding: tracedecay_contracts::WorkProductBindingV1,
        revisions: tracedecay_contracts::WorkProductRevisionPinsV1,
        topology: tracedecay_domain::WorkTopologyPolicyV1,
        command: tracedecay_contracts::RetryWorkAttemptCommandV1,
        restarted_at: tracedecay_domain::UtcMicros,
        workflow_rebind: Option<tracedecay_contracts::WorkflowFanOutRetryRebindV1>,
    ) -> Result<
        tracedecay_contracts::WorkRetryAttemptOutcomeV1,
        tracedecay_contracts::ApplicationProblem,
    > {
        let prefetch_context = context.clone();
        let observed_at = restarted_at;
        let graph = self
            .storage
            .prefetch_graph(move |storage| {
                tracedecay_contracts::PrefetchedWorkGraphReadV1::for_attempt_admission(
                    storage,
                    &prefetch_context,
                    observed_at,
                )
            })
            .await;
        self.storage
            .transact_with_graph(graph, move |storage| {
                tracedecay_contracts::WorkProductRetryServiceV1::new(
                    storage,
                    tracedecay_contracts::RuntimeWorkRetryEvidenceV1,
                )
                .retry(
                    &context,
                    &binding,
                    &revisions,
                    &topology,
                    command.clone(),
                    restarted_at,
                    workflow_rebind.clone(),
                )
            })
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
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
        Ok(Self::over(db.work_storage()?))
    }

    fn over(storage: tracedecay_rusqlite_runtime::work::WorkSqliteStorage) -> Self {
        Self {
            attempts: tracedecay_contracts::WorkAttemptService::new(storage.clone()),
            run_control: tracedecay_contracts::WorkRunControlService::new(storage.clone()),
            placement: tracedecay_contracts::WorkPlacementService::new(storage.clone()),
            artifact_hydration: tracedecay_contracts::WorkArtifactHydrationService::new(
                storage.clone(),
            ),
            duplicate_adjudications: tracedecay_contracts::WorkDuplicateAdjudicationServiceV1::new(
                storage.clone(),
            ),
            storage,
        }
    }

    /// Runs the synchronous Work reads in `read` on the blocking pool, so the
    /// awaiting task parks no runtime thread. A read that cannot run is the
    /// attempt authority's unavailable problem.
    pub async fn read<R>(
        &self,
        read: impl FnOnce(&Self) -> Result<R, tracedecay_contracts::ApplicationProblem> + Send + 'static,
    ) -> Result<R, tracedecay_contracts::ApplicationProblem>
    where
        R: Send + 'static,
    {
        self.storage
            .read(move |storage| read(&Self::over(storage.clone())))
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
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

    /// Fences every open attempt after a restart as a single writer
    /// transaction: the census and every fence commit together or not at all.
    pub async fn resume_attempts(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::ResumeWorkAttemptsCommand,
    ) -> Result<
        tracedecay_contracts::WorkAttemptRecoveryReportV1,
        tracedecay_contracts::ApplicationProblem,
    > {
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkAttemptService::new(storage).resume(&context, &command)
            })
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
    }

    /// Runs one provider-driver step of the attempt authority as a single
    /// writer transaction: its load, its decision and its fenced update
    /// commit together.
    async fn attempt_step<R>(
        &self,
        step: impl Fn(
            &tracedecay_contracts::WorkAttemptService<
                tracedecay_rusqlite_runtime::work_run_control::WorkSqliteJobStorage<'_>,
            >,
        ) -> Result<R, tracedecay_contracts::ApplicationProblem>
        + Send
        + Sync
        + 'static,
    ) -> Result<R, tracedecay_contracts::ApplicationProblem>
    where
        R: Send + 'static,
    {
        self.storage
            .transact(move |storage| step(&tracedecay_contracts::WorkAttemptService::new(storage)))
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
    }

    /// Records a cancellation request against an open attempt as a single
    /// writer transaction: the load, the conflict check and the fenced update
    /// commit together.
    pub async fn cancel_attempt(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::CancelWorkAttemptCommand,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        self.attempt_step(move |attempts| attempts.request_cancellation(&context, command.clone()))
            .await
    }

    /// Marks the provider running under `actual_route`.
    pub async fn mark_running(
        &self,
        context: tracedecay_contracts::RequestContext,
        identity: tracedecay_domain::WorkAttemptIdentityV1,
        actual_route: tracedecay_domain::WorkProviderRouteV1,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        self.attempt_step(move |attempts| {
            attempts.mark_running(&context, &identity, actual_route.clone())
        })
        .await
    }

    /// Fences an attempt whose provider could not start for recovery.
    pub async fn mark_provider_unavailable(
        &self,
        context: tracedecay_contracts::RequestContext,
        identity: tracedecay_domain::WorkAttemptIdentityV1,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        self.attempt_step(move |attempts| {
            attempts.mark_provider_unavailable(&context, &identity, observed_at)
        })
        .await
    }

    /// Fails a recovery-required attempt with sealed denial evidence.
    pub async fn fail_recovery(
        &self,
        context: tracedecay_contracts::RequestContext,
        identity: tracedecay_domain::WorkAttemptIdentityV1,
        evidence: tracedecay_contracts::WorkAttemptEvidenceRecordV1,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        self.attempt_step(move |attempts| attempts.fail_recovery(&context, &identity, &evidence))
            .await
    }

    /// Acknowledges a durable cancellation request.
    pub async fn acknowledge_cancellation(
        &self,
        context: tracedecay_contracts::RequestContext,
        identity: tracedecay_domain::WorkAttemptIdentityV1,
        acknowledged_at: tracedecay_domain::UtcMicros,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        self.attempt_step(move |attempts| {
            attempts.acknowledge_cancellation(&context, &identity, acknowledged_at)
        })
        .await
    }

    /// Escalates an acknowledged cancellation to forced termination.
    pub async fn escalate_cancellation(
        &self,
        context: tracedecay_contracts::RequestContext,
        identity: tracedecay_domain::WorkAttemptIdentityV1,
        escalated_at: tracedecay_domain::UtcMicros,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        self.attempt_step(move |attempts| {
            attempts.escalate_cancellation(&context, &identity, escalated_at)
        })
        .await
    }

    /// Seals an attempt with terminal evidence and its artifacts.
    pub async fn settle_with_artifacts(
        &self,
        context: tracedecay_contracts::RequestContext,
        identity: tracedecay_domain::WorkAttemptIdentityV1,
        evidence: tracedecay_contracts::WorkAttemptEvidenceRecordV1,
        artifacts: Vec<tracedecay_domain::WorkArtifactRefV1>,
    ) -> Result<tracedecay_domain::WorkAttemptV1, tracedecay_contracts::ApplicationProblem> {
        self.attempt_step(move |attempts| {
            attempts.settle_with_artifacts(&context, &identity, &evidence, artifacts.clone())
        })
        .await
    }

    /// Records the immutable dispatch receipt before a provider launch, as a
    /// single writer transaction.
    pub async fn record_effect_dispatch(
        &self,
        context: tracedecay_contracts::RequestContext,
        attempt: tracedecay_domain::WorkAttemptIdentityV1,
        effect_state: tracedecay_domain::WorkEffectStateV1,
        dispatched_at: tracedecay_domain::UtcMicros,
        deadline: tracedecay_domain::UtcMicros,
    ) -> Result<
        tracedecay_contracts::WorkAttemptEffectDispatchOutcomeV1,
        tracedecay_contracts::ApplicationProblem,
    > {
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkAttemptEffectServiceV1::new(storage).record_dispatch(
                    &context,
                    attempt.clone(),
                    effect_state,
                    dispatched_at,
                    deadline,
                )
            })
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
    }

    /// Settles the dispatch receipt's effect resolution, as a single writer
    /// transaction.
    pub async fn settle_effect_dispatch(
        &self,
        context: tracedecay_contracts::RequestContext,
        attempt: tracedecay_domain::WorkAttemptIdentityV1,
        resolution: tracedecay_contracts::WorkAttemptEffectResolutionV1,
        resolved_at: tracedecay_domain::UtcMicros,
    ) -> Result<
        tracedecay_contracts::WorkAttemptEffectHolderV1,
        tracedecay_contracts::ApplicationProblem,
    > {
        self.storage
            .transact(move |storage| {
                tracedecay_contracts::WorkAttemptEffectServiceV1::new(storage).settle(
                    &context,
                    &attempt,
                    resolution,
                    resolved_at,
                )
            })
            .await
            .unwrap_or_else(|_| Err(attempt_unavailable()))
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
    /// reads the target (Git) on the blocking pool before the job, never on
    /// the writer or the caller's worker.
    pub async fn admit_placement(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::AdmitWorkPlacementCommand,
        observe: impl FnOnce(
            &tracedecay_domain::WorkPlacementTargetV1,
        ) -> Result<
            tracedecay_domain::WorkPlacementObservationV1,
            tracedecay_contracts::ApplicationProblem,
        > + Send
        + 'static,
    ) -> Result<tracedecay_domain::WorkPlacementV1, tracedecay_contracts::ApplicationProblem> {
        let observation = observe_placement(observe, command.target.clone()).await?;
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
    /// version, so the Git observation never runs on the writer. It runs on
    /// the blocking pool, never on the caller's worker.
    pub async fn release_placement(
        &self,
        context: tracedecay_contracts::RequestContext,
        command: tracedecay_contracts::ReleaseWorkPlacementCommand,
        observe: impl FnOnce(
            &tracedecay_domain::WorkPlacementTargetV1,
        ) -> Result<
            tracedecay_domain::WorkPlacementObservationV1,
            tracedecay_contracts::ApplicationProblem,
        > + Send
        + 'static,
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
        let observed_target = current.target().clone();
        let observation = observe_placement(observe, observed_target.clone()).await?;
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
    pub async fn attach(db: &RegisteredGlobalDb) -> tracedecay_domain::errors::Result<Self> {
        let authority = db.workflow_storage().await?;
        let runtime = db.project_graph_runtime().cloned().ok_or_else(|| {
            attach_error(
                "attach registered workflow topology",
                "project graph runtime is not bound",
            )
        })?;
        Ok(Self::over(RegisteredWorkflowTopologyV1 {
            source: authority,
            runtime,
        }))
    }

    fn over(topology: RegisteredWorkflowTopologyV1) -> Self {
        Self {
            definitions: tracedecay_contracts::WorkflowDefinitionService::new(
                topology.source.clone(),
            ),
            effects: topology.source.clone(),
            topology,
        }
    }

    /// Runs the synchronous Workflow reads in `read` on the blocking pool, so
    /// the awaiting task parks no runtime thread. A read that cannot run is
    /// the Workflow runtime's unavailable problem.
    pub async fn read<R>(
        &self,
        read: impl FnOnce(&Self) -> Result<R, tracedecay_contracts::ApplicationProblem> + Send + 'static,
    ) -> Result<R, tracedecay_contracts::ApplicationProblem>
    where
        R: Send + 'static,
    {
        let topology = self.topology.clone();
        self.effects
            .read(move |_| read(&Self::over(topology)))
            .await
            .unwrap_or_else(|_| Err(workflow_unavailable()))
    }

    /// Journals one Workflow effect as a single writer transaction: the
    /// reservation, the claim, the mutation, the terminal and its
    /// reconciliation commit together or not at all.
    pub async fn execute_effect(
        &self,
        identity: tracedecay_contracts::WorkflowEffectIdentityV1,
        prepared: tracedecay_contracts::WorkflowEffectPreparedV1,
        ended_at: tracedecay_domain::UtcMicros,
    ) -> Result<
        tracedecay_contracts::WorkflowEffectJournalRecordV1,
        tracedecay_contracts::WorkflowEffectAuthorityErrorV1,
    > {
        self.effects
            .transact(move |storage| {
                tracedecay_contracts::WorkflowEffectAuthorityPortV1::execute_effect(
                    &storage, &identity, &prepared, ended_at,
                )
            })
            .await
            .unwrap_or_else(|error| {
                Err(
                    tracedecay_contracts::WorkflowEffectAuthorityErrorV1::Unavailable(
                        error.to_string(),
                    ),
                )
            })
    }

    /// Admits one run, with its fan-out plans, as a single writer
    /// transaction.
    pub async fn admit_run(
        &self,
        run_id: tracedecay_domain::RunId,
        definition: tracedecay_domain::WorkflowDefinition,
        admission: tracedecay_contracts::WorkflowAdmissionSnapshot,
        fan_out_plans: Vec<tracedecay_domain::WorkflowFanOutPlanV1>,
        context: tracedecay_domain::WorkflowRunEventContext,
    ) -> Result<
        tracedecay_domain::WorkflowRunProjection,
        tracedecay_contracts::WorkflowRunServiceError,
    > {
        self.effects
            .transact(move |storage| {
                tracedecay_contracts::WorkflowRunService::new(storage).admit_with_fan_out(
                    run_id.clone(),
                    definition.clone(),
                    admission.clone(),
                    fan_out_plans.clone(),
                    context.clone(),
                )
            })
            .await
            .unwrap_or_else(|_| {
                Err(tracedecay_contracts::WorkflowRunStorageError::Unavailable.into())
            })
    }

    /// Applies one run command as a single writer transaction: the
    /// projection read, the sequence check and the append commit together.
    pub async fn apply_run(
        &self,
        run_id: tracedecay_domain::RunId,
        expected_sequence: u64,
        command: tracedecay_domain::WorkflowRunCommand,
        context: tracedecay_domain::WorkflowRunEventContext,
    ) -> Result<
        tracedecay_domain::WorkflowRunProjection,
        tracedecay_contracts::WorkflowRunServiceError,
    > {
        self.effects
            .transact(move |storage| {
                tracedecay_contracts::WorkflowRunService::new(storage).apply(
                    &run_id,
                    expected_sequence,
                    command.clone(),
                    context.clone(),
                )
            })
            .await
            .unwrap_or_else(|_| {
                Err(tracedecay_contracts::WorkflowRunStorageError::Unavailable.into())
            })
    }

    /// Persists one fan-out census as a single writer transaction, checked
    /// against the run journal it names.
    pub async fn persist_census(
        &self,
        census: tracedecay_domain::WorkflowFanOutCensusV1,
    ) -> Result<
        tracedecay_contracts::WorkflowFanOutCensusPersistOutcomeV1,
        tracedecay_contracts::WorkflowFanOutCensusError,
    > {
        self.effects
            .transact(move |storage| {
                tracedecay_contracts::WorkflowFanOutCensusStoragePort::persist_census(
                    &storage, &census,
                )
            })
            .await
            .unwrap_or(Err(
                tracedecay_contracts::WorkflowFanOutCensusError::Unavailable,
            ))
    }

    /// Marks every census in `censuses` observability-durable as one writer
    /// transaction.
    pub async fn mark_censuses_observability_durable(
        &self,
        censuses: Vec<tracedecay_domain::WorkflowFanOutCensusV1>,
    ) -> Result<(), tracedecay_contracts::WorkflowFanOutCensusError> {
        self.effects
            .transact(move |storage| {
                censuses.iter().try_for_each(|census| {
                    tracedecay_contracts::WorkflowFanOutCensusStoragePort::mark_census_observability_durable(
                        &storage, census,
                    )
                })
            })
            .await
            .unwrap_or(Err(tracedecay_contracts::WorkflowFanOutCensusError::Unavailable))
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
        self.effects.has_pending_effects(worktree_id)
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

async fn observe_placement(
    observe: impl FnOnce(
        &tracedecay_domain::WorkPlacementTargetV1,
    ) -> Result<
        tracedecay_domain::WorkPlacementObservationV1,
        tracedecay_contracts::ApplicationProblem,
    > + Send
    + 'static,
    target: tracedecay_domain::WorkPlacementTargetV1,
) -> Result<tracedecay_domain::WorkPlacementObservationV1, tracedecay_contracts::ApplicationProblem>
{
    tracedecay_runtime_core::native_runtime::spawn_blocking(move || observe(&target))
        .await
        .unwrap_or_else(|_| Err(placement_unavailable()))
}

fn run_control_unavailable() -> tracedecay_contracts::ApplicationProblem {
    tracedecay_contracts::WorkRunControlStorageError::Unavailable.into()
}

fn attempt_unavailable() -> tracedecay_contracts::ApplicationProblem {
    tracedecay_contracts::WorkAttemptStorageError::Unavailable.into()
}

fn workflow_unavailable() -> tracedecay_contracts::ApplicationProblem {
    tracedecay_contracts::ApplicationProblem::unavailable(tracedecay_contracts::SafeDiagnostic {
        code: "workflow.unavailable".to_owned(),
        message: "The Workflow application runtime is unavailable".to_owned(),
    })
}

fn attach_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}
