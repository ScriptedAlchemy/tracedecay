use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

use tokio::task::JoinHandle;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    CalibrationProfileId, CodeGenerationId, ComponentRevision, SemanticSearchIndexProfileV1,
    VectorGenerationIdV1, canonical_sha256,
};
use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_semantic_contracts::SemanticFallbackReasonV1;

use crate::config::retrieval::RetrievalRuntimeCompatibilityV1;
use crate::config::retrieval::{
    RetrievalCompatibilityPinsV1, SemanticCompatibilityPinsV1, SemanticResourceRequirementV1,
};
use crate::search_eval::semantic_native::{
    SemanticNativePendingReasonV1, SemanticNativeResourceProvenanceV1,
    SemanticNativeResourceSampleV1, SemanticNativeStageResultV1, SemanticProjectionCaseSampleV1,
    SemanticProjectionCaseV1,
};
use crate::search_eval::{
    CandidateOutputError, ProductionCandidateNativeExecutionAuthorityV1,
    ProductionCandidateNativeGenerationResourcesV1, ProductionCandidateNativeQueryContextV1,
    ProductionCandidateNativeQueryInputsV1, ProductionCandidateNativeResourceContextV1,
    evaluate_default_activation_candidate,
};
use tracedecay_usecases::semantic_runtime::{
    SemanticActivationCoordinationErrorV1, SemanticEvaluationAuthorityPublicationV1,
    SemanticEvaluationProfileCandidateV1, SemanticEvaluationPublicationSnapshotPortV1,
    SemanticEvaluationPublicationSnapshotV1, SemanticEvaluationSnapshotPortV1,
    SemanticRuntimeBackendErrorV1, SemanticRuntimeFuture,
};
use tracedecay_usecases::store::vector_generations::{
    BaseGenerationIncompatibilityV1, GraphVectorGenerationStoreV1, PublishedVectorGenerationV1,
    VectorGenerationStoreErrorV1,
};

use crate::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use crate::semantic_evaluation_shutdown::{
    SemanticEvaluationShutdownJoinV1, SemanticEvaluationShutdownReceiptV1,
};

static RESOURCE_MEASUREMENT_LOCK_V1: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

const EVALUATION_ACTIVE: u8 = 0;
const EVALUATION_CANCELLED: u8 = 1;
const EVALUATION_COMMIT_STARTED: u8 = 2;
const EVALUATION_TIMED_OUT: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonSemanticEvaluationExecutionErrorV1 {
    Cancelled,
    TimedOut,
    Coordination(SemanticActivationCoordinationErrorV1),
}

pub struct DaemonSemanticEvaluationControlV1 {
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
    phase: AtomicU8,
}

impl DaemonSemanticEvaluationControlV1 {
    fn new(cancellation: CancellationToken, deadline: tokio::time::Instant) -> Self {
        Self {
            cancellation,
            deadline,
            phase: AtomicU8::new(EVALUATION_ACTIVE),
        }
    }

    fn cancel(&self) -> bool {
        self.transition_to(EVALUATION_CANCELLED)
    }

    fn expire(&self) -> bool {
        self.transition_to(EVALUATION_TIMED_OUT)
    }

    fn transition_to(&self, terminal_phase: u8) -> bool {
        let interrupted = self
            .phase
            .compare_exchange(
                EVALUATION_ACTIVE,
                terminal_phase,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if interrupted {
            self.cancellation.cancel();
        }
        interrupted
    }

    fn execution_error(
        &self,
        fallback: SemanticActivationCoordinationErrorV1,
    ) -> DaemonSemanticEvaluationExecutionErrorV1 {
        if self.cancellation.is_cancelled() {
            self.cancel();
        }
        match self.phase.load(Ordering::Acquire) {
            EVALUATION_CANCELLED => DaemonSemanticEvaluationExecutionErrorV1::Cancelled,
            EVALUATION_TIMED_OUT => DaemonSemanticEvaluationExecutionErrorV1::TimedOut,
            _ => DaemonSemanticEvaluationExecutionErrorV1::Coordination(fallback),
        }
    }

    fn checkpoint(&self) -> Result<(), SemanticActivationCoordinationErrorV1> {
        if self.cancellation.is_cancelled() {
            self.cancel();
        }
        if tokio::time::Instant::now() >= self.deadline {
            self.expire();
        }
        match self.phase.load(Ordering::Acquire) {
            EVALUATION_CANCELLED | EVALUATION_TIMED_OUT => {
                Err(SemanticActivationCoordinationErrorV1::Unavailable)
            }
            _ => Ok(()),
        }
    }

    fn try_begin_commit(&self) -> Result<(), SemanticActivationCoordinationErrorV1> {
        self.checkpoint()?;
        self.phase
            .compare_exchange(
                EVALUATION_ACTIVE,
                EVALUATION_COMMIT_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)
    }

    pub async fn interruptible<Output>(
        &self,
        operation: impl Future<Output = Output>,
    ) -> Result<Output, SemanticActivationCoordinationErrorV1> {
        self.checkpoint()?;
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                self.cancel();
                Err(SemanticActivationCoordinationErrorV1::Unavailable)
            }
            () = tokio::time::sleep_until(self.deadline) => {
                self.expire();
                Err(SemanticActivationCoordinationErrorV1::Unavailable)
            }
            output = operation => {
                self.checkpoint()?;
                Ok(output)
            }
        }
    }
}

impl tracedecay_semantic::SemanticExecutionAuthority for DaemonSemanticEvaluationControlV1 {
    fn interruption(&self) -> Option<tracedecay_semantic::SemanticExecutionInterruptionV1> {
        self.checkpoint().err()?;
        match self.phase.load(Ordering::Acquire) {
            EVALUATION_CANCELLED => {
                Some(tracedecay_semantic::SemanticExecutionInterruptionV1::Cancelled)
            }
            EVALUATION_TIMED_OUT => {
                Some(tracedecay_semantic::SemanticExecutionInterruptionV1::DeadlineExceeded)
            }
            _ => None,
        }
    }
}

impl tracedecay_semantic::SemanticEvaluationCancellationV1 for DaemonSemanticEvaluationControlV1 {}

/// Wait for one semantic task without detaching it when request cancellation
/// or deadline wins. `interruptible` drops only its borrowed awaiter, while
/// this owner keeps and joins the task after it propagates the linked
/// cancellation token into semantic execution.
async fn await_semantic_task<Output>(
    control: &DaemonSemanticEvaluationControlV1,
    task: &mut JoinHandle<Result<Output, SemanticActivationCoordinationErrorV1>>,
) -> Result<Output, SemanticActivationCoordinationErrorV1> {
    let result = match control.interruptible(&mut *task).await {
        Ok(result) => result.map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?,
        Err(error) => {
            let _ = task.await;
            return Err(error);
        }
    };
    control.checkpoint()?;
    result
}

fn coordination_error_from_runtime(
    error: SemanticRuntimeBackendErrorV1,
) -> SemanticActivationCoordinationErrorV1 {
    match error {
        SemanticRuntimeBackendErrorV1::Unavailable => {
            SemanticActivationCoordinationErrorV1::Unavailable
        }
        SemanticRuntimeBackendErrorV1::Rejected => {
            SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic runtime rejected the verified evaluation target authority".to_owned(),
            )
        }
        SemanticRuntimeBackendErrorV1::Conflict => SemanticActivationCoordinationErrorV1::Conflict,
    }
}

pub async fn build_daemon_semantic_evaluation_candidate(
    project_root: &Path,
    scope: &ResolvedScope,
    scheduler: &CodeIndexSchedulerRegistryV1,
    evaluated_profile_id: &str,
    control: Arc<DaemonSemanticEvaluationControlV1>,
) -> Result<SemanticEvaluationProfileCandidateV1, SemanticActivationCoordinationErrorV1> {
    control.checkpoint()?;
    let snapshot = control
        .interruptible(hotpath::future!(
            scheduler.semantic_evaluation_snapshot_for_scope(scope),
            label = "daemon.semantic.evaluation.candidate.code_snapshot"
        ))
        .await?
        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
    let serving = control
        .interruptible(hotpath::future!(
            scheduler.serving_code_scope(project_root),
            label = "daemon.semantic.evaluation.candidate.serving_code"
        ))
        .await?
        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
    let code = serving
        .serving_generation
        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
    if code.manifest().generation_id != snapshot.source_generation
        || code.projection().request().changes.manifest_digest != snapshot.source_manifest_digest
        || code.manifest().snapshot_digest != snapshot.snapshot_digest
        || code.capability().manifest_digest != snapshot.capability_manifest_digest
    {
        return Err(SemanticActivationCoordinationErrorV1::Conflict);
    }

    let status = hotpath::measure_block!(
        "daemon.semantic.evaluation.candidate.runtime_status",
        tracedecay_usecases::semantic_runtime::project_semantic_application_status(
            project_root,
            None,
        )
    )
    .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
    let vector_generation_id = semantic_publication_generation(&status.state)?;
    let provider = control
        .interruptible(hotpath::future!(
            scheduler.semantic_vector_graph_provider(project_root),
            label = "daemon.semantic.evaluation.candidate.vector_provider"
        ))
        .await?
        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
    let retained = control
        .interruptible(hotpath::future!(
            provider.graph_for_generation(&code),
            label = "daemon.semantic.evaluation.candidate.vector_graph"
        ))
        .await?
        .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
    let store = hotpath::measure_block!(
        "daemon.semantic.evaluation.candidate.vector_store",
        GraphVectorGenerationStoreV1::read_only_generation(&retained, &vector_generation_id)
    )
    .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?
    .ok_or(SemanticActivationCoordinationErrorV1::Conflict)?;
    let vector = control
        .interruptible(hotpath::future!(
            store.generation(&vector_generation_id, Arc::clone(retained.cancellation())),
            label = "daemon.semantic.evaluation.candidate.vector_generation"
        ))
        .await
        .inspect_err(|_| {
            hotpath::measure_block!(
                "daemon.semantic.evaluation.candidate.vector_generation.control_interrupted",
                ()
            );
        })?
        .map_err(|error| {
            record_vector_generation_failure(&error);
            tracing::warn!(
                error = %error,
                "semantic evaluation candidate vector generation is unavailable"
            );
            SemanticActivationCoordinationErrorV1::Unavailable
        })?;
    let Some(vector) = vector else {
        hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.missing",
            ()
        );
        return Err(SemanticActivationCoordinationErrorV1::Conflict);
    };
    // The vector manifest identifies the canonical semantic chunk corpus,
    // while the code snapshot manifest identifies the projection change set.
    // Their digests are intentionally different authorities; the shared code
    // generation binds both without comparing unlike manifest domains.
    if vector.source_generation() != &snapshot.source_generation {
        tracing::warn!(
            source_generation_matches = vector.source_generation() == &snapshot.source_generation,
            "semantic evaluation candidate vector identity conflicts with the code snapshot"
        );
        return Err(SemanticActivationCoordinationErrorV1::Conflict);
    }
    let runtime = hotpath::measure_block!(
        "daemon.semantic.evaluation.candidate.production_runtime",
        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(project_root)
    );
    let Some(runtime) = runtime else {
        tracing::warn!("semantic evaluation candidate production runtime is unavailable");
        return Err(SemanticActivationCoordinationErrorV1::Unavailable);
    };
    let resources = hotpath::measure_block!(
        "daemon.semantic.evaluation.candidate.resource_requirement",
        runtime.evaluation_target_resource_requirement()
    )
    .map_err(|error| {
        tracing::warn!(
            ?error,
            "semantic evaluation candidate resource requirement is unavailable"
        );
        coordination_error_from_runtime(error)
    })?;
    hotpath::measure_block!(
        "daemon.semantic.evaluation.candidate.materialize",
        daemon_semantic_evaluation_candidate(evaluated_profile_id, &code, &vector, resources)
    )
}

fn record_vector_generation_failure(error: &VectorGenerationStoreErrorV1) {
    match error {
        VectorGenerationStoreErrorV1::Cancelled => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.cancelled",
            ()
        ),
        VectorGenerationStoreErrorV1::DeadlineExceeded => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.deadline",
            ()
        ),
        VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
            BaseGenerationIncompatibilityV1::MissingPublished,
        ) => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.base_missing_published",
            ()
        ),
        VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
            BaseGenerationIncompatibilityV1::IdentityMismatch,
        ) => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.base_identity",
            ()
        ),
        VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
            BaseGenerationIncompatibilityV1::MissingSnapshot,
        ) => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.base_missing_snapshot",
            ()
        ),
        VectorGenerationStoreErrorV1::Corrupt(_) => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.corrupt",
            ()
        ),
        VectorGenerationStoreErrorV1::Unavailable(_) => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.unavailable",
            ()
        ),
        VectorGenerationStoreErrorV1::Storage(_) => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.storage",
            ()
        ),
        VectorGenerationStoreErrorV1::ConcurrentMutation(_) => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.concurrent_mutation",
            ()
        ),
        _ => hotpath::measure_block!(
            "daemon.semantic.evaluation.candidate.vector_generation.failed.other",
            ()
        ),
    }
}

pub fn semantic_publication_generation(
    state: &tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1,
) -> Result<VectorGenerationIdV1, SemanticActivationCoordinationErrorV1> {
    use tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1;

    match state {
        SemanticRuntimeStateV1::Current { receipt } => Ok(receipt.activated_generation.clone()),
        SemanticRuntimeStateV1::Degraded {
            active_generation: Some(_),
            reason: SemanticFallbackReasonV1::Stale,
        }
        | SemanticRuntimeStateV1::Rollback { .. } => {
            Err(SemanticActivationCoordinationErrorV1::Conflict)
        }
        SemanticRuntimeStateV1::Degraded {
            active_generation: Some(generation),
            ..
        } => Ok(generation.clone()),
        _ => Err(SemanticActivationCoordinationErrorV1::Unavailable),
    }
}

fn daemon_semantic_evaluation_candidate(
    evaluated_profile_id: &str,
    code: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    vector: &PublishedVectorGenerationV1,
    resources: SemanticResourceRequirementV1,
) -> Result<SemanticEvaluationProfileCandidateV1, SemanticActivationCoordinationErrorV1> {
    let material = crate::search_eval::load_default_evaluated_profile_material(
        evaluated_profile_id,
    )
    .map_err(|_| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(
            "semantic evaluation profile is not in the packaged workload".to_owned(),
        )
    })?;
    let embedding = vector.embedding_key().embedding_key();
    let runtime_compatibility_digest = canonical_sha256(&(
        "tracedecay.semantic-runtime-compatibility.v1",
        &embedding.runtime_backend,
        &embedding.runtime_build_revision,
        embedding.device_class,
        embedding.precision,
    ))
    .map_err(|_| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(
            "semantic evaluation runtime compatibility digest is invalid".to_owned(),
        )
    })?;
    let search_index_key = SemanticSearchIndexProfileV1::exact_flat_v1()
        .and_then(|profile| profile.index_key())
        .map_err(|_| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic evaluation search index identity is invalid".to_owned(),
            )
        })?;
    let vector_generation_id = vector.generation_id().clone();
    let calibration_profile_id = evaluated_semantic_calibration_profile_id(&material)?;
    let calibration = SemanticCalibrationProfileV1 {
        calibration_profile_id,
        cohort_digest: canonical_sha256(&(
            "tracedecay.semantic.evaluation-calibration-cohort.v1",
            code.manifest().generation_id.clone(),
            vector.source_manifest_digest().clone(),
            code.capability().manifest_digest.clone(),
            vector.embedding_key().clone(),
            vector_generation_id.clone(),
            embedding.model_artifact_digest.clone(),
        ))
        .map_err(|_| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic evaluation calibration cohort digest is invalid".to_owned(),
            )
        })?,
        projection_key: vector.projection_key().clone(),
        vector_generation: vector_generation_id.clone(),
        capability_manifest_digest: code.capability().manifest_digest.clone(),
        // Measured from this generation's own vectors rather than guessed. The
        // certifying reader recomputes the identical bound from the same
        // immutable generation, so exact-equality certification still holds.
        maximum_distance_micros:
            tracedecay_usecases::semantic_runtime::measure_acceptance_calibration(vector.vectors())
                .maximum_distance_micros,
        minimum_margin_micros: 0,
    };
    Ok(SemanticEvaluationProfileCandidateV1 {
        evaluated_profile_id: evaluated_profile_id.to_owned(),
        profile: tracedecay_usecases::semantic_runtime::SemanticEvaluationFusionCandidateV1 {
            profile_id: material.profile.profile_id.clone(),
            calibrations: material.profile.calibrations.clone(),
            score_domain_calibrations: material.profile.score_domain_calibrations.clone(),
            minimum_calibrated_feature_micros: material
                .profile
                .minimum_calibrated_feature_micros
                .clone(),
            weights_micros: material.profile.weights_micros.clone(),
            diversity_policy_id: material.profile.diversity_policy_id.clone(),
            rerank_policy_id: material.profile.rerank_policy_id.clone(),
            retrieval_budget: material.profile.retrieval_budget,
        },
        diversity: tracedecay_usecases::semantic_runtime::SemanticEvaluationDiversityCandidateV1 {
            policy_id: material.diversity.policy_id.clone(),
            per_source_namespace: material.diversity.per_source_namespace,
            per_source_instance: material.diversity.per_source_instance,
            per_repository: material.diversity.per_repository,
            per_file: material.diversity.per_file,
            per_session_or_thread: material.diversity.per_session_or_thread,
            per_copy_cluster: material.diversity.per_copy_cluster,
            per_evidence_role: material.diversity.per_evidence_role,
        },
        rerank: None,
        compatibility: RetrievalCompatibilityPinsV1 {
            semantic: Some(SemanticCompatibilityPinsV1 {
                implementation_revision: ComponentRevision::new("semantic.fastembed.production.v1")
                    .map_err(|_| {
                        SemanticActivationCoordinationErrorV1::RejectedDetail(
                            "semantic evaluation implementation revision is invalid".to_owned(),
                        )
                    })?,
                fusion_revision: ComponentRevision::new(
                    tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1,
                )
                .map_err(|_| {
                    SemanticActivationCoordinationErrorV1::RejectedDetail(
                        "semantic evaluation fusion revision is invalid".to_owned(),
                    )
                })?,
                artifact_manifest_digest: embedding.model_artifact_digest.clone(),
                runtime_compatibility_digest,
                projection: vector.embedding_key().clone(),
                search_index_key,
                vector_generation_id,
                calibration,
                resources,
            }),
            rerank: None,
        },
        semantic_source_manifest_digest: Some(vector.source_manifest_digest().clone()),
    })
}

fn evaluated_semantic_calibration_profile_id(
    material: &crate::search_eval::DirectEvaluatedProfileMaterialV1,
) -> Result<CalibrationProfileId, SemanticActivationCoordinationErrorV1> {
    material
        .profile
        .calibrations
        .get(&tracedecay_domain::RetrieverKind::Semantic)
        .cloned()
        .ok_or_else(|| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic evaluation profile has no semantic calibration identity".to_owned(),
            )
        })
}

struct SemanticEvaluationWorkerV1 {
    control: Arc<DaemonSemanticEvaluationControlV1>,
    handle: JoinHandle<()>,
}

struct SemanticEvaluationWorkersV1 {
    accepting: bool,
    next_sequence: u64,
    workers: BTreeMap<u64, SemanticEvaluationWorkerV1>,
}

impl Default for SemanticEvaluationWorkersV1 {
    fn default() -> Self {
        Self {
            accepting: true,
            next_sequence: 0,
            workers: BTreeMap::new(),
        }
    }
}

pub struct DaemonSemanticEvaluationWorkerOwnerV1 {
    workers: Mutex<SemanticEvaluationWorkersV1>,
    scheduler_admission: Arc<tokio::sync::Semaphore>,
}

impl Default for DaemonSemanticEvaluationWorkerOwnerV1 {
    fn default() -> Self {
        Self::with_scheduler_admission(Arc::new(tokio::sync::Semaphore::new(1)))
    }
}

struct SemanticEvaluationActiveGaugeV1;

impl SemanticEvaluationActiveGaugeV1 {
    fn enter() -> Self {
        hotpath::gauge!("search_eval_active_workers").inc(1.0);
        Self
    }
}

impl Drop for SemanticEvaluationActiveGaugeV1 {
    fn drop(&mut self) {
        hotpath::gauge!("search_eval_active_workers").dec(1.0);
    }
}

impl DaemonSemanticEvaluationWorkerOwnerV1 {
    pub fn with_scheduler_admission(scheduler_admission: Arc<tokio::sync::Semaphore>) -> Self {
        Self {
            workers: Mutex::new(SemanticEvaluationWorkersV1::default()),
            scheduler_admission,
        }
    }

    #[hotpath::measure(label = "daemon.semantic.evaluation.execute", future = true)]
    pub async fn execute<Output, Work, WorkFuture>(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
        admitted_cancellation: CancellationToken,
        work: Work,
    ) -> Result<Output, DaemonSemanticEvaluationExecutionErrorV1>
    where
        Output: Send + 'static,
        Work: FnOnce(Arc<DaemonSemanticEvaluationControlV1>) -> WorkFuture + Send + 'static,
        WorkFuture:
            Future<Output = Result<Output, SemanticActivationCoordinationErrorV1>> + Send + 'static,
    {
        let cancellation = admitted_cancellation.child_token();
        let control = Arc::new(DaemonSemanticEvaluationControlV1::new(
            cancellation.clone(),
            deadline,
        ));
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let (sequence, result_control) = {
            let mut workers = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if control.checkpoint().is_err() {
                return Err(
                    control.execution_error(SemanticActivationCoordinationErrorV1::Unavailable)
                );
            }
            if !workers.accepting {
                return Err(DaemonSemanticEvaluationExecutionErrorV1::Coordination(
                    SemanticActivationCoordinationErrorV1::Unavailable,
                ));
            }
            let sequence = workers.next_sequence.checked_add(1).ok_or(
                DaemonSemanticEvaluationExecutionErrorV1::Coordination(
                    SemanticActivationCoordinationErrorV1::Unavailable,
                ),
            )?;
            workers.next_sequence = sequence;
            let worker_control = Arc::clone(&control);
            let result_control = Arc::clone(&control);
            let scheduler_admission = Arc::clone(&self.scheduler_admission);
            let handle = tokio::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                let mut evaluation = Box::pin(async {
                    let _scheduler_admission = worker_control
                        .interruptible(hotpath::future!(
                            scheduler_admission.acquire_owned(),
                            label = "search_eval.daemon.scheduler_admission_wait"
                        ))
                        .await?
                        .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
                    let _active = SemanticEvaluationActiveGaugeV1::enter();
                    hotpath::future!(
                        work(Arc::clone(&worker_control)),
                        label = "search_eval.daemon.worker"
                    )
                    .await
                });
                let outcome = tokio::select! {
                    result = &mut evaluation => {
                        result.map_err(|error| worker_control.execution_error(error))
                    }
                    () = worker_control.cancellation.cancelled() => {
                        if worker_control.cancel() {
                            let _ = evaluation.await;
                            Err(worker_control.execution_error(
                                SemanticActivationCoordinationErrorV1::Unavailable,
                            ))
                        } else {
                            evaluation
                                .await
                                .map_err(|error| worker_control.execution_error(error))
                        }
                    }
                    () = tokio::time::sleep_until(deadline) => {
                        if worker_control.expire() {
                            let _ = evaluation.await;
                            Err(DaemonSemanticEvaluationExecutionErrorV1::TimedOut)
                        } else {
                            evaluation
                                .await
                                .map_err(|error| worker_control.execution_error(error))
                        }
                    }
                };
                let _ = result_tx.send(outcome);
            });
            workers
                .workers
                .insert(sequence, SemanticEvaluationWorkerV1 { control, handle });
            (sequence, result_control)
        };
        let _ = start_tx.send(());
        let outcome = result_rx.await.map_err(|_| {
            result_control.execution_error(SemanticActivationCoordinationErrorV1::Unavailable)
        });
        self.join_finished(sequence).await;
        outcome?
    }

    async fn join_finished(&self, sequence: u64) {
        let handle = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .workers
            .remove(&sequence)
            .map(|worker| worker.handle);
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    pub async fn cancel_and_join_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> SemanticEvaluationShutdownReceiptV1 {
        let mut pending = {
            let mut workers = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            workers.accepting = false;
            let pending = std::mem::take(&mut workers.workers);
            for worker in pending.values() {
                worker.control.cancel();
            }
            pending
        };
        let mut joined_workers = 0;
        let mut failed_workers = 0;
        let sequences = pending.keys().copied().collect::<Vec<_>>();
        for sequence in sequences {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            let Some(worker) = pending.get_mut(&sequence) else {
                continue;
            };
            match tokio::time::timeout_at(deadline, &mut worker.handle).await {
                Ok(Ok(())) => {
                    pending.remove(&sequence);
                    joined_workers += 1;
                }
                Ok(Err(_join_error)) => {
                    pending.remove(&sequence);
                    failed_workers += 1;
                }
                Err(_) => break,
            }
        }
        let remaining_workers = pending.len();
        if !pending.is_empty() {
            self.workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .workers
                .extend(pending);
        }
        SemanticEvaluationShutdownReceiptV1 {
            joined_workers,
            failed_workers,
            remaining_workers,
        }
    }
}

impl SemanticEvaluationShutdownJoinV1 for DaemonSemanticEvaluationWorkerOwnerV1 {
    fn cancel_and_join_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> std::pin::Pin<Box<dyn Future<Output = SemanticEvaluationShutdownReceiptV1> + Send + '_>>
    {
        Box::pin(DaemonSemanticEvaluationWorkerOwnerV1::cancel_and_join_until(self, deadline))
    }
}

/// Identity of an isolated projection-case measurement: the clean generation
/// plus the three mutation sources it is measured against. The measurement is a
/// pure function of exactly these four generations — it never reads the profile
/// or the partition — so every pass that shares them shares its result.
type SemanticProjectionCaseKeyV1 = (
    CodeGenerationId,
    CodeGenerationId,
    CodeGenerationId,
    CodeGenerationId,
);

/// Identity of an incremental projection measurement: the clean generation it
/// is prepared from plus the incremental generation it rebuilds into. Like the
/// projection-case measurement, it is a pure function of exactly these two
/// generations and never reads the profile or the partition.
type SemanticIncrementalProjectionKeyV1 = (CodeGenerationId, CodeGenerationId);

#[derive(Clone)]
pub struct DaemonSemanticEvaluationSnapshotAuthorityV1 {
    project_root: PathBuf,
    scope: ResolvedScope,
    scheduler: CodeIndexSchedulerRegistryV1,
    candidate: SemanticEvaluationProfileCandidateV1,
    control: Arc<DaemonSemanticEvaluationControlV1>,
    projection_batch_cache: Arc<tracedecay_semantic::SemanticEvaluationProjectionBatchCacheV1>,
    prepared_native: Arc<
        Mutex<
            BTreeMap<
                CodeGenerationId,
                Arc<tracedecay_usecases::semantic_runtime::PreparedSemanticEvaluationGenerationV1>,
            >,
        >,
    >,
    projection_cases: Arc<
        Mutex<
            BTreeMap<
                SemanticProjectionCaseKeyV1,
                BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
            >,
        >,
    >,
    incremental_projections: Arc<
        Mutex<
            BTreeMap<
                SemanticIncrementalProjectionKeyV1,
                ProductionCandidateNativeGenerationResourcesV1,
            >,
        >,
    >,
}

impl DaemonSemanticEvaluationSnapshotAuthorityV1 {
    pub fn new(
        project_root: PathBuf,
        scope: ResolvedScope,
        scheduler: CodeIndexSchedulerRegistryV1,
        candidate: SemanticEvaluationProfileCandidateV1,
        control: Arc<DaemonSemanticEvaluationControlV1>,
    ) -> Self {
        Self {
            project_root,
            scope,
            scheduler,
            candidate,
            control,
            projection_batch_cache: Arc::new(
                tracedecay_semantic::SemanticEvaluationProjectionBatchCacheV1::new(),
            ),
            prepared_native: Arc::new(Mutex::new(BTreeMap::new())),
            projection_cases: Arc::new(Mutex::new(BTreeMap::new())),
            incremental_projections: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

/// The only daemon authority carrying the publication capability. The
/// qualification execution path receives the inner snapshot authority instead,
/// so it cannot reach compare-and-swap publication or configuration bootstrap.
pub struct DaemonSemanticEvaluationPublicationAuthorityV1 {
    snapshot: DaemonSemanticEvaluationSnapshotAuthorityV1,
}

impl DaemonSemanticEvaluationPublicationAuthorityV1 {
    pub fn new(snapshot: DaemonSemanticEvaluationSnapshotAuthorityV1) -> Self {
        Self { snapshot }
    }
}

fn semantic_projection_pin_mismatch(
    prepared: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    pinned: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
) -> CandidateOutputError {
    let prepared = prepared.embedding_key();
    let pinned = pinned.embedding_key();
    CandidateOutputError::Contract(format!(
        "semantic resource projection does not match candidate pins: prepared chunker={} privacy={}; pinned chunker={} privacy={}",
        prepared.chunker_revision,
        prepared.privacy_domain,
        pinned.chunker_revision,
        pinned.privacy_domain,
    ))
}

impl ProductionCandidateNativeExecutionAuthorityV1 for DaemonSemanticEvaluationSnapshotAuthorityV1 {
    #[hotpath::measure(label = "daemon.semantic.evaluation.with_query_inputs")]
    fn with_query_inputs(
        &self,
        context: ProductionCandidateNativeQueryContextV1<'_>,
        evaluate: &mut dyn for<'inputs> FnMut(
            ProductionCandidateNativeQueryInputsV1<'inputs>,
        ) -> Result<(), CandidateOutputError>,
    ) -> Result<(), CandidateOutputError> {
        self.control.checkpoint().map_err(|_| {
            CandidateOutputError::Contract("semantic evaluation was cancelled".to_owned())
        })?;
        if context.profile.semantic_weight_ppm == 0 {
            return evaluate(ProductionCandidateNativeQueryInputsV1 {
                semantic: None,
                rerank: None,
            });
        }
        let required = self
            .candidate
            .compatibility
            .semantic
            .as_ref()
            .ok_or_else(|| {
                CandidateOutputError::Contract(
                    "semantic evaluator profile has no admitted production runtime".to_owned(),
                )
            })?;
        let mut prepared = self.prepared_native.lock().map_err(|_| {
            CandidateOutputError::Contract(
                "semantic evaluator generation cache is unavailable".to_owned(),
            )
        })?;
        if !prepared.contains_key(context.code_generation) {
            let runtime =
                tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                    &self.project_root,
                )
                .ok_or_else(|| {
                    CandidateOutputError::Contract(
                        "production semantic runtime is unavailable".to_owned(),
                    )
                })?;
            let generation = hotpath::measure_block!("search_eval.projection.clean", {
                runtime.prepare_evaluation_generation_with_cache(
                    context.code,
                    Arc::clone(&self.projection_batch_cache),
                    Arc::clone(&self.control)
                        as Arc<dyn tracedecay_semantic::SemanticEvaluationCancellationV1>,
                )
            })
            .map_err(|error| CandidateOutputError::Contract(format!("{error:?}")))?;
            if generation.projection() != &required.projection {
                return Err(semantic_projection_pin_mismatch(
                    generation.projection(),
                    &required.projection,
                ));
            }
            prepared.insert(context.code_generation.clone(), Arc::new(generation));
        }
        let generation = prepared.get(context.code_generation).ok_or_else(|| {
            CandidateOutputError::Contract(
                "semantic evaluator generation cache lost its prepared generation".to_owned(),
            )
        })?;
        if generation.projection() != &required.projection {
            return Err(semantic_projection_pin_mismatch(
                generation.projection(),
                &required.projection,
            ));
        }
        let rerank_authority = self
            .candidate
            .compatibility
            .rerank
            .as_ref()
            .and_then(|pins| {
                crate::semantic_code::shared_lifecycle_owner()
                    .and_then(|owner| owner.mount_reranker(pins.clone()).ok())
            });
        let result = hotpath::measure_block!("search_eval.native_query.inputs", {
            generation.with_query_inputs(context, rerank_authority.as_ref(), evaluate)
        });
        self.control.checkpoint().map_err(|_| {
            CandidateOutputError::Contract("semantic evaluation was cancelled".to_owned())
        })?;
        result
    }

    #[hotpath::measure(label = "daemon.semantic.evaluation.measure_resources")]
    fn measure_resources(
        &self,
        context: ProductionCandidateNativeResourceContextV1<'_>,
        execute_queries: &mut dyn FnMut() -> Result<Vec<u64>, CandidateOutputError>,
    ) -> Result<SemanticNativeStageResultV1<SemanticNativeResourceSampleV1>, CandidateOutputError>
    {
        self.control.checkpoint().map_err(|_| {
            CandidateOutputError::Contract("semantic evaluation was cancelled".to_owned())
        })?;
        let semantic_resources = self.candidate.compatibility.semantic.as_ref();
        if let Some(required) = semantic_resources {
            let mut prepared = self.prepared_native.lock().map_err(|_| {
                CandidateOutputError::Contract(
                    "semantic evaluator generation cache is unavailable".to_owned(),
                )
            })?;
            if !prepared.contains_key(context.code_generation) {
                let runtime =
                    tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                        &self.project_root,
                    )
                    .ok_or_else(|| {
                        CandidateOutputError::Contract(
                            "production semantic runtime is unavailable".to_owned(),
                        )
                    })?;
                let generation = hotpath::measure_block!("search_eval.projection.clean", {
                    runtime.prepare_evaluation_generation_with_cache(
                        context.code,
                        Arc::clone(&self.projection_batch_cache),
                        Arc::clone(&self.control)
                            as Arc<dyn tracedecay_semantic::SemanticEvaluationCancellationV1>,
                    )
                })
                .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
                if generation.projection() != &required.projection {
                    return Err(semantic_projection_pin_mismatch(
                        generation.projection(),
                        &required.projection,
                    ));
                }
                prepared.insert(context.code_generation.clone(), Arc::new(generation));
            }
        }
        let resource_window = LinuxProcessResourceWindowV1::begin();
        let latency_samples_us =
            hotpath::measure_block!("search_eval.resource_measurement", execute_queries())?;
        self.control.checkpoint().map_err(|_| {
            CandidateOutputError::Contract("semantic evaluation was cancelled".to_owned())
        })?;
        let process_resources = resource_window.and_then(LinuxProcessResourceWindowV1::finish);
        let resources = if semantic_resources.is_some() {
            let prepared = self.prepared_native.lock().map_err(|_| {
                CandidateOutputError::Contract(
                    "semantic evaluator generation cache is unavailable".to_owned(),
                )
            })?;
            let prepared = prepared.get(context.code_generation).ok_or_else(|| {
                CandidateOutputError::Contract(
                    "semantic resource measurement has no prepared generation".to_owned(),
                )
            })?;
            let runtime =
                tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                    &self.project_root,
                )
                .ok_or_else(|| {
                    CandidateOutputError::Contract(
                        "production semantic runtime is unavailable".to_owned(),
                    )
                })?;
            // The incremental projection measurement reloads the semantic
            // artifact and re-projects the incremental generation's changed
            // chunks. Like the projection-case measurement below it reads only
            // generations — never the profile or the partition — so the six
            // passes that share a scale were each rebuilding the identical
            // observation. Every field it produces is a property of that
            // generation pair (model bytes, thread and batch ceilings, cold
            // load, clean build, incremental rebuild), and each pass records
            // them as its own single-element sample vectors with no averaging
            // across passes, so sharing one observation changes no reported
            // quantity's meaning.
            let incremental_key: SemanticIncrementalProjectionKeyV1 = (
                context.code_generation.clone(),
                context.incremental_code.manifest().generation_id.clone(),
            );
            let cached_incremental = self
                .incremental_projections
                .lock()
                .map_err(|_| {
                    CandidateOutputError::Contract(
                        "semantic incremental projection cache is unavailable".to_owned(),
                    )
                })?
                .get(&incremental_key)
                .cloned();
            let mut resources = match cached_incremental {
                Some(resources) => resources,
                None => {
                    // Measured without the cache lock held, for the same
                    // reason as the projection cases: long, idempotent work
                    // where a race should duplicate once rather than block.
                    let measured = hotpath::measure_block!(
                        "search_eval.projection.incremental",
                        runtime.measure_incremental_evaluation_projection(
                            prepared,
                            context.incremental_code,
                        )
                    )
                    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

                    self.incremental_projections
                        .lock()
                        .map_err(|_| {
                            CandidateOutputError::Contract(
                                "semantic incremental projection cache is unavailable".to_owned(),
                            )
                        })?
                        .entry(incremental_key)
                        .or_insert(measured)
                        .clone()
                }
            };
            // The isolated projection-case measurement stands up a fresh
            // TempDir, graph, and metadata store and re-projects the whole
            // corpus, so it is the most expensive term in a pass. It depends
            // only on the four generations below, while the driver runs one
            // pass per profile x partition x scale — so without this memo the
            // identical measurement is rebuilt once per profile and partition
            // at each scale. Keyed exactly like the `prepared_native` cache
            // above, it collapses to one measurement per distinct input set.
            let projection_cases_key: SemanticProjectionCaseKeyV1 = (
                context.code_generation.clone(),
                context
                    .semantic_projection_sources
                    .one_symbol
                    .manifest()
                    .generation_id
                    .clone(),
                context
                    .semantic_projection_sources
                    .no_op
                    .manifest()
                    .generation_id
                    .clone(),
                context
                    .semantic_projection_sources
                    .deletion
                    .manifest()
                    .generation_id
                    .clone(),
            );
            let cached = self
                .projection_cases
                .lock()
                .map_err(|_| {
                    CandidateOutputError::Contract(
                        "semantic projection case cache is unavailable".to_owned(),
                    )
                })?
                .get(&projection_cases_key)
                .cloned();
            resources.projection_cases = match cached {
                Some(cases) => cases,
                None => {
                    // Measured without the cache lock held: the work is long
                    // and idempotent, so a racing pass may duplicate it once
                    // rather than block, and the first result installed wins.
                    let measured = hotpath::measure_block!(
                        "search_eval.projection.cases",
                        runtime.measure_evaluation_projection_cases(
                            prepared,
                            &context.semantic_projection_sources,
                        )
                    )
                    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

                    self.projection_cases
                        .lock()
                        .map_err(|_| {
                            CandidateOutputError::Contract(
                                "semantic projection case cache is unavailable".to_owned(),
                            )
                        })?
                        .entry(projection_cases_key)
                        .or_insert(measured)
                        .clone()
                }
            };
            resources
        } else {
            return Ok(SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::ResourceMeasurementUnavailable,
            });
        };
        self.control.checkpoint().map_err(|_| {
            CandidateOutputError::Contract("semantic evaluation was cancelled".to_owned())
        })?;
        let mismatch = if resources.source_generation != *context.code_generation {
            Some("source_generation")
        } else if resources.source_manifest_digest
            != context.code.projection().request().changes.manifest_digest
        {
            Some("source_manifest_digest")
        } else if resources.incremental_source_generation
            != context.incremental_code.manifest().generation_id
        {
            Some("incremental_source_generation")
        } else if resources.incremental_source_manifest_digest
            != context
                .incremental_code
                .projection()
                .request()
                .changes
                .manifest_digest
        {
            Some("incremental_source_manifest_digest")
        } else if semantic_resources.is_some() && resources.model_bytes == 0 {
            Some("model_bytes")
        } else if semantic_resources.is_some() && resources.tokenizer_bytes == 0 {
            Some("tokenizer_bytes")
        } else if semantic_resources.is_some() && resources.threads == 0 {
            Some("threads")
        } else if semantic_resources.is_some() && resources.batch_size == 0 {
            Some("batch_size")
        } else if semantic_resources.is_some() && resources.sequence_length == 0 {
            Some("sequence_length")
        } else if semantic_resources.is_some() && resources.load_deadline_ms == 0 {
            Some("load_deadline_ms")
        } else if semantic_resources.is_some() && resources.cold_model_load_micros == 0 {
            Some("cold_model_load_micros")
        } else if semantic_resources.is_some() && resources.vector_bytes == 0 {
            Some("vector_bytes")
        } else if semantic_resources.is_some() && resources.projection_cases.len() != 7 {
            Some("projection_cases")
        } else {
            None
        };
        if let Some(mismatch) = mismatch {
            return Err(CandidateOutputError::Contract(format!(
                "semantic resource measurement is not bound to the exact prepared generation: {mismatch}"
            )));
        }
        let Some((cpu_time_us, peak_rss_bytes)) = process_resources else {
            return Ok(SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::ResourceMeasurementUnavailable,
            });
        };
        Ok(SemanticNativeStageResultV1::Complete(
            SemanticNativeResourceSampleV1 {
                provenance: SemanticNativeResourceProvenanceV1 {
                    workload_digest: context.workload_digest.to_owned(),
                    corpus_digest: context.corpus_digest.to_owned(),
                    scale: context.scale.to_owned(),
                    code_generation_id: resources.source_generation.as_str().to_owned(),
                    code_source_manifest_digest: resources
                        .source_manifest_digest
                        .as_str()
                        .to_owned(),
                    incremental_code_generation_id: resources
                        .incremental_source_generation
                        .as_str()
                        .to_owned(),
                    incremental_code_source_manifest_digest: resources
                        .incremental_source_manifest_digest
                        .as_str()
                        .to_owned(),
                    incremental_before_content_digest: context
                        .incremental_before_content_digest
                        .to_owned(),
                    incremental_after_content_digest: context
                        .incremental_after_content_digest
                        .to_owned(),
                    threads: resources.threads,
                    max_concurrent_sessions: resources.max_concurrent_sessions,
                    batch_size: resources.batch_size,
                    sequence_length: resources.sequence_length,
                    load_deadline_ms: resources.load_deadline_ms,
                    vector_generation_id: resources
                        .vector_generation
                        .as_ref()
                        .map(|generation| generation.as_digest().as_str().to_owned()),
                    artifact_digest: resources
                        .artifact_digest
                        .as_ref()
                        .map(|digest| digest.as_str().to_owned()),
                    measurement_method: "linux-procfs-v1:cpu=/proc/self/stat(utime+stime,getconf-CLK_TCK);rss=/proc/self/status(VmHWM-process-lifetime-peak);query/clean-build/incremental/stages/projection-cases=std::time::Instant;projection-cases=prepare_semantic_evaluation_projection+verified-publication-required;hydration=canonical-late-hydration+authorized-fixture-filesystem-reads+receipt-count;model+tokenizer=catalog-verified-member-lengths;execution=admitted-fastembed-runtime-settings;cold-load=session-pool-monotonic-duration-with-enforced-deadline;vector=sum-f32-bytes;index=exact-flat-zero;cache=session-pool-resident-bytes"
                        .to_owned(),
                },
                eligible_chunks: context.eligible_chunks,
                measured_queries: latency_samples_us.len() as u64,
                latency_samples_us,
                cpu_time_us: Some(cpu_time_us),
                peak_rss_bytes: Some(peak_rss_bytes),
                model_bytes: Some(resources.model_bytes),
                tokenizer_bytes: Some(resources.tokenizer_bytes),
                vector_bytes: Some(resources.vector_bytes),
                index_bytes: Some(resources.index_bytes),
                cache_bytes: Some(resources.cache_bytes),
                cold_model_load_samples_us: vec![resources.cold_model_load_micros],
                clean_projection_build_samples_us: vec![resources.clean_projection_build_micros],
                incremental_rebuild_samples_us: vec![resources.incremental_rebuild_micros],
                projection_cases: resources.projection_cases,
            },
        ))
    }
}

impl SemanticEvaluationSnapshotPortV1 for DaemonSemanticEvaluationSnapshotAuthorityV1 {
    fn current(
        &self,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<SemanticEvaluationPublicationSnapshotV1, SemanticActivationCoordinationErrorV1>,
    > {
        Box::pin(hotpath::future!(
            async move {
                self.control.checkpoint()?;
                let code = self
                    .control
                    .interruptible(hotpath::future!(
                        self.scheduler
                            .semantic_evaluation_snapshot_for_scope(&self.scope),
                        label = "daemon.semantic.evaluation.snapshot.code"
                    ))
                    .await?
                    .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
                let (
                    semantic_source_generation,
                    semantic_source_manifest_digest,
                    semantic_ceiling,
                    vector_state_revision,
                    vector_generation_id,
                    semantic,
                    semantic_lifecycle_verification,
                ) = match self.candidate.compatibility.semantic.as_ref() {
                    Some(candidate) => {
                        let semantic_source_manifest_digest = self
                            .candidate
                            .semantic_source_manifest_digest
                            .clone()
                            .ok_or_else(|| {
                                SemanticActivationCoordinationErrorV1::RejectedDetail(
                                    "semantic evaluation candidate omits its vector corpus manifest"
                                        .to_owned(),
                                )
                            })?;
                        let runtime =
                        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                            &self.project_root,
                        )
                        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
                        let candidate = candidate.clone();
                        let source_generation = code.source_generation.clone();
                        let verified_source_manifest_digest =
                            semantic_source_manifest_digest.clone();
                        let capability_manifest_digest = code.capability_manifest_digest.clone();
                        let cancellation = Arc::clone(&self.control)
                            as Arc<dyn tracedecay_semantic::SemanticEvaluationCancellationV1>;
                        let mut verification = tokio::spawn(hotpath::future!(
                            async move {
                                runtime
                                    .inspect_verified_evaluation_target_snapshot(
                                        &candidate,
                                        &source_generation,
                                        &verified_source_manifest_digest,
                                        &capability_manifest_digest,
                                        cancellation,
                                    )
                                    .await
                                    .map_err(coordination_error_from_runtime)
                            },
                            label = "daemon.semantic.evaluation.snapshot.verify_target"
                        ));
                        let receipt = await_semantic_task(&self.control, &mut verification).await?;
                        (
                            Some(code.source_generation.clone()),
                            Some(semantic_source_manifest_digest),
                            Some(receipt.configured_resource_ceiling()),
                            Some(receipt.vector_state_revision()),
                            Some(receipt.vector_generation_id().clone()),
                            Some(receipt.semantic_compatibility().clone()),
                            Some(receipt.lifecycle_verification().clone()),
                        )
                    }
                    None => (None, None, None, None, None, None, None),
                };
                let evaluated = hotpath::measure_block!(
                    "daemon.semantic.evaluation.snapshot.profile_material",
                    crate::search_eval::load_default_evaluated_profile_material(
                        &self.candidate.evaluated_profile_id,
                    )
                )
                .map_err(|error| {
                    SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                        "semantic evaluation profile material is unavailable: {error}"
                    ))
                })?;
                self.control.checkpoint()?;
                Ok(SemanticEvaluationPublicationSnapshotV1 {
                    project_root: self.project_root.clone(),
                    scope: self.scope.clone(),
                    code_generation: code.source_generation,
                    code_source_manifest_digest: code.source_manifest_digest,
                    code_snapshot_digest: code.snapshot_digest,
                    code_capability_manifest_digest: code.capability_manifest_digest,
                    semantic_source_generation,
                    semantic_source_manifest_digest,
                    vector_state_revision,
                    vector_generation_id,
                    semantic_lifecycle_verification,
                    runtime: RetrievalRuntimeCompatibilityV1 {
                        retrieval_ceiling:
                            super::code_index_scheduler::queries::maximum_retrieval_budget(),
                        semantic,
                        semantic_ceiling,
                        rerank: self.candidate.compatibility.rerank.clone(),
                        rerank_ceiling: evaluated.rerank,
                    },
                })
            },
            label = "daemon.semantic.evaluation.snapshot.current"
        ))
    }

    fn evaluate_default_candidate<'a>(
        &'a self,
        evaluated_profile_id: &'a str,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<
            crate::search_eval::DirectActivationEvaluationV1,
            SemanticActivationCoordinationErrorV1,
        >,
    > {
        let authority = self.clone();
        let evaluated_profile_id = evaluated_profile_id.to_owned();
        Box::pin(async move {
            authority.control.checkpoint()?;
            let measurement = authority
                .control
                .interruptible(RESOURCE_MEASUREMENT_LOCK_V1.acquire())
                .await?
                .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
            let mut task = tokio::task::spawn_blocking(move || {
                let _measurement = measurement;
                authority.control.checkpoint()?;
                evaluate_default_activation_candidate(&evaluated_profile_id, &authority).map_err(
                    |error| {
                        SemanticActivationCoordinationErrorV1::RejectedDetail(error.to_string())
                    },
                )
            });
            await_semantic_task(&self.control, &mut task).await
        })
    }
}

impl SemanticEvaluationSnapshotPortV1 for DaemonSemanticEvaluationPublicationAuthorityV1 {
    fn current(
        &self,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<SemanticEvaluationPublicationSnapshotV1, SemanticActivationCoordinationErrorV1>,
    > {
        self.snapshot.current()
    }

    fn evaluate_default_candidate<'a>(
        &'a self,
        evaluated_profile_id: &'a str,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<
            crate::search_eval::DirectActivationEvaluationV1,
            SemanticActivationCoordinationErrorV1,
        >,
    > {
        self.snapshot
            .evaluate_default_candidate(evaluated_profile_id)
    }
}

impl SemanticEvaluationPublicationSnapshotPortV1
    for DaemonSemanticEvaluationPublicationAuthorityV1
{
    fn publish_if_current<'a>(
        &'a self,
        expected: &'a SemanticEvaluationPublicationSnapshotV1,
        publication: SemanticEvaluationAuthorityPublicationV1,
    ) -> SemanticRuntimeFuture<'a, Result<(), SemanticActivationCoordinationErrorV1>> {
        Box::pin(hotpath::future!(
            async move {
                self.snapshot.control.checkpoint()?;
                let expected_code = super::code_index_scheduler::SemanticEvaluationCodeSnapshotV1 {
                    source_generation: expected.code_generation.clone(),
                    source_manifest_digest: expected.code_source_manifest_digest.clone(),
                    snapshot_digest: expected.code_snapshot_digest.clone(),
                    capability_manifest_digest: expected.code_capability_manifest_digest.clone(),
                };
                let code_lease = self
                    .snapshot
                    .control
                    .interruptible(hotpath::future!(
                        self.snapshot
                            .scheduler
                            .acquire_semantic_evaluation_publication_lease(
                                &self.snapshot.scope,
                                &expected_code,
                            ),
                        label = "daemon.semantic.evaluation.publish.code_lease"
                    ))
                    .await?;
                let Some(_code_lease) = code_lease else {
                    return Err(SemanticActivationCoordinationErrorV1::Conflict);
                };
                let semantic_lifecycle_verification =
                    expected.semantic_lifecycle_verification.clone();
                let vector_state_revision = expected.vector_state_revision;
                let vector_generation_id = expected.vector_generation_id.clone();
                let runtime = match (
                    semantic_lifecycle_verification,
                    vector_state_revision,
                    vector_generation_id,
                ) {
                    (Some(verification), Some(revision), Some(generation)) => Some((
                        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                            &self.snapshot.project_root,
                        )
                        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?,
                        verification,
                        revision,
                        generation,
                    )),
                    (None, None, None) => None,
                    (verification, revision, generation) => {
                        return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
                            format!(
                                "semantic evaluation snapshot is internally inconsistent: \
                                 lifecycle_verification={} vector_state_revision={} \
                                 vector_generation={}",
                                verification.is_some(),
                                revision.is_some(),
                                generation.is_some(),
                            ),
                        ));
                    }
                };
                let _vector_lease = match runtime.as_ref() {
                    Some((runtime, _, revision, generation)) => Some(
                        self.snapshot
                            .control
                            .interruptible(hotpath::future!(
                                runtime.acquire_vector_publication_lease(*revision, generation),
                                label = "daemon.semantic.evaluation.publish.vector_lease"
                            ))
                            .await?
                            .map_err(|_| SemanticActivationCoordinationErrorV1::Conflict)?,
                    ),
                    None => None,
                };
                let _lifecycle_lease = if let Some((runtime, verification, _, _)) = runtime.as_ref()
                {
                    let runtime = runtime.clone();
                    let verification = verification.clone();
                    let cancellation = Arc::clone(&self.snapshot.control)
                        as Arc<dyn tracedecay_semantic::SemanticEvaluationCancellationV1>;
                    let mut acquisition = tokio::spawn(hotpath::future!(
                        async move {
                            runtime
                                .acquire_verified_evaluation_target_publication_lease(
                                    &verification,
                                    cancellation,
                                )
                                .await
                                .map_err(coordination_error_from_runtime)
                        },
                        label = "daemon.semantic.evaluation.publish.lifecycle_lease"
                    ));
                    Some(await_semantic_task(&self.snapshot.control, &mut acquisition).await?)
                } else {
                    None
                };
                self.snapshot.control.try_begin_commit()?;
                let result = hotpath::future!(
                    publication.commit(expected),
                    label = "daemon.semantic.evaluation.publish.commit"
                )
                .await;
                self.snapshot.control.checkpoint()?;
                result
            },
            label = "daemon.semantic.evaluation.publish.total"
        ))
    }
}

struct LinuxProcessResourceWindowV1 {
    cpu_ticks: u64,
    ticks_per_second: u64,
}

impl LinuxProcessResourceWindowV1 {
    #[cfg(target_os = "linux")]
    fn begin() -> Option<Self> {
        Some(Self {
            cpu_ticks: tracedecay_session_memory::runtime_telemetry::read_linux_process_cpu_ticks(
            )?,
            ticks_per_second:
                tracedecay_session_memory::runtime_telemetry::linux_clock_ticks_per_second()?,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn begin() -> Option<Self> {
        None
    }

    fn finish(self) -> Option<(u64, u64)> {
        let elapsed_ticks =
            tracedecay_session_memory::runtime_telemetry::read_linux_process_cpu_ticks()?
                .saturating_sub(self.cpu_ticks);
        let cpu_time_us = u64::try_from(
            u128::from(elapsed_ticks)
                .checked_mul(1_000_000)?
                .checked_div(u128::from(self.ticks_per_second))?,
        )
        .ok()?;
        Some((cpu_time_us, read_linux_process_lifetime_peak_rss_bytes()?))
    }
}

#[cfg(not(target_os = "linux"))]
fn read_linux_process_lifetime_peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn read_linux_process_lifetime_peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1_024)
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn packaged_candidate_uses_its_evaluated_semantic_calibration() {
        let material =
            crate::search_eval::load_default_evaluated_profile_material("hybrid-conservative")
                .expect("packaged semantic profile");
        let evaluated = material
            .profile
            .calibrations
            .get(&tracedecay_domain::RetrieverKind::Semantic)
            .expect("semantic calibration");

        assert_eq!(
            evaluated_semantic_calibration_profile_id(&material)
                .expect("candidate semantic calibration"),
            evaluated.clone()
        );
    }

    #[tokio::test]
    async fn shutdown_cancels_and_joins_evaluation_prepare() {
        let owner = Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let execution = {
            let owner = Arc::clone(&owner);
            tokio::spawn(async move {
                owner
                    .execute(
                        tokio::time::Instant::now() + Duration::from_secs(5),
                        CancellationToken::new(),
                        move |control| async move {
                            let _ = started_tx.send(());
                            while control.checkpoint().is_ok() {
                                tokio::task::yield_now().await;
                            }
                            Err::<(), _>(SemanticActivationCoordinationErrorV1::Unavailable)
                        },
                    )
                    .await
            })
        };
        started_rx.await.expect("evaluation prepare started");

        let receipt = owner
            .cancel_and_join_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;

        assert!(receipt.is_clean());
        assert_eq!(receipt.joined_workers, 1);
        assert_eq!(
            execution.await.expect("execution task"),
            Err(DaemonSemanticEvaluationExecutionErrorV1::Cancelled)
        );
    }

    #[tokio::test]
    async fn shutdown_reports_a_panicked_worker_as_failed_not_clean() {
        let owner = Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let execution = {
            let owner = Arc::clone(&owner);
            tokio::spawn(async move {
                owner
                    .execute(
                        tokio::time::Instant::now() + Duration::from_secs(5),
                        CancellationToken::new(),
                        move |control| async move {
                            let _ = started_tx.send(());
                            while control.checkpoint().is_ok() {
                                tokio::task::yield_now().await;
                            }
                            panic!("semantic evaluation worker crashed during shutdown");
                            #[allow(unreachable_code)]
                            Err::<(), _>(SemanticActivationCoordinationErrorV1::Unavailable)
                        },
                    )
                    .await
            })
        };
        started_rx.await.expect("evaluation prepare started");

        let receipt = owner
            .cancel_and_join_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;

        assert!(!receipt.is_clean());
        assert_eq!(receipt.failed_workers, 1);
        assert_eq!(receipt.joined_workers, 0);
        assert_eq!(receipt.remaining_workers, 0);
        assert!(execution.await.expect("execution task").is_err());
    }

    #[tokio::test]
    async fn evaluation_deadline_returns_a_typed_timeout() {
        let owner = Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::default());
        let result = owner
            .execute(
                tokio::time::Instant::now() + Duration::from_millis(5),
                CancellationToken::new(),
                |control| async move {
                    while control.checkpoint().is_ok() {
                        tokio::task::yield_now().await;
                    }
                    Err::<(), _>(SemanticActivationCoordinationErrorV1::Unavailable)
                },
            )
            .await;

        assert_eq!(
            result,
            Err(DaemonSemanticEvaluationExecutionErrorV1::TimedOut)
        );
    }

    #[tokio::test]
    async fn evaluation_waits_for_scheduler_admission_and_times_out_typed() {
        let admission = Arc::new(tokio::sync::Semaphore::new(0));
        let owner =
            Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::with_scheduler_admission(admission));
        let work_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_work_started = Arc::clone(&work_started);

        let result = owner
            .execute(
                tokio::time::Instant::now() + Duration::from_millis(5),
                CancellationToken::new(),
                move |_control| async move {
                    observed_work_started.store(true, Ordering::Release);
                    Ok::<(), SemanticActivationCoordinationErrorV1>(())
                },
            )
            .await;

        assert_eq!(
            result,
            Err(DaemonSemanticEvaluationExecutionErrorV1::TimedOut)
        );
        assert!(
            !work_started.load(Ordering::Acquire),
            "semantic evaluation work must not start without scheduler admission"
        );
    }

    #[tokio::test]
    async fn evaluation_waiting_for_scheduler_admission_cancels_typed() {
        let admission = Arc::new(tokio::sync::Semaphore::new(0));
        let owner =
            Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::with_scheduler_admission(admission));
        let request_cancellation = CancellationToken::new();
        let work_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_work_started = Arc::clone(&work_started);
        let execution = {
            let owner = Arc::clone(&owner);
            let request_cancellation = request_cancellation.clone();
            tokio::spawn(async move {
                owner
                    .execute(
                        tokio::time::Instant::now() + Duration::from_secs(5),
                        request_cancellation,
                        move |_control| async move {
                            observed_work_started.store(true, Ordering::Release);
                            Ok::<(), SemanticActivationCoordinationErrorV1>(())
                        },
                    )
                    .await
            })
        };

        tokio::task::yield_now().await;
        request_cancellation.cancel();

        assert_eq!(
            execution.await.expect("evaluation task"),
            Err(DaemonSemanticEvaluationExecutionErrorV1::Cancelled)
        );
        assert!(
            !work_started.load(Ordering::Acquire),
            "cancelled semantic evaluation must not bypass scheduler admission"
        );
    }

    #[tokio::test]
    async fn shutdown_waits_for_an_effect_commit_and_returns_a_clean_receipt() {
        let owner = Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::default());
        let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let execution = {
            let owner = Arc::clone(&owner);
            tokio::spawn(async move {
                owner
                    .execute(
                        tokio::time::Instant::now() + Duration::from_secs(5),
                        CancellationToken::new(),
                        move |control| async move {
                            control.try_begin_commit()?;
                            let _ = commit_tx.send(());
                            let _ = release_rx.await;
                            Ok::<_, SemanticActivationCoordinationErrorV1>(())
                        },
                    )
                    .await
            })
        };
        commit_rx.await.expect("effect commit started");
        let shutdown = {
            let owner = Arc::clone(&owner);
            tokio::spawn(async move {
                owner
                    .cancel_and_join_until(tokio::time::Instant::now() + Duration::from_secs(1))
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished(), "commit worker must be joined");
        release_tx.send(()).expect("release effect commit");

        let receipt = shutdown.await.expect("shutdown task");
        assert!(receipt.is_clean());
        assert_eq!(receipt.joined_workers, 1);
        assert_eq!(execution.await.expect("execution task"), Ok(()));
    }

    #[tokio::test]
    async fn request_cancellation_joins_native_evaluation_worker_before_returning() {
        let owner = Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::default());
        let request_cancellation = CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
        let mut execution = {
            let owner = Arc::clone(&owner);
            let request_cancellation = request_cancellation.clone();
            tokio::spawn(async move {
                owner
                    .execute(
                        tokio::time::Instant::now() + Duration::from_secs(5),
                        request_cancellation,
                        move |control| async move {
                            let child_control = Arc::clone(&control);
                            let mut child = tokio::task::spawn_blocking(move || {
                                let _ = started_tx.send(());
                                while child_control.checkpoint().is_ok() {
                                    std::thread::yield_now();
                                }
                                let _ = finished_tx.send(());
                                Err::<(), _>(SemanticActivationCoordinationErrorV1::Unavailable)
                            });
                            await_semantic_task(&control, &mut child).await
                        },
                    )
                    .await
            })
        };
        started_rx.await.expect("native evaluation started");

        request_cancellation.cancel();

        tokio::select! {
            biased;
            _ = &mut finished_rx => {}
            result = &mut execution => panic!(
                "cancelled response returned before native blocking child completed: {result:?}"
            ),
        }
        assert_eq!(
            execution.await.expect("execution task"),
            Err(DaemonSemanticEvaluationExecutionErrorV1::Cancelled)
        );
        let receipt = owner
            .cancel_and_join_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;
        assert!(receipt.is_clean());
        assert_eq!(receipt.joined_workers, 0);
        assert_eq!(receipt.remaining_workers, 0);
    }

    #[tokio::test]
    async fn deadline_joins_blocking_model_preflight_before_timing_out() {
        let control = Arc::new(DaemonSemanticEvaluationControlV1::new(
            CancellationToken::new(),
            tokio::time::Instant::now() + Duration::from_secs(5),
        ));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
        let mut preflight = {
            let control = Arc::clone(&control);
            tokio::spawn(async move {
                let child_control = Arc::clone(&control);
                let mut child = tokio::task::spawn_blocking(move || {
                    let _ = started_tx.send(());
                    while child_control.checkpoint().is_ok() {
                        std::thread::yield_now();
                    }
                    let _ = finished_tx.send(());
                    Err::<(), _>(SemanticActivationCoordinationErrorV1::Unavailable)
                });
                await_semantic_task(&control, &mut child).await
            })
        };
        started_rx.await.expect("model preflight started");

        assert!(control.expire(), "preflight deadline must win exactly once");
        tokio::select! {
            biased;
            _ = &mut finished_rx => {}
            result = &mut preflight => panic!(
                "timeout returned before blocking model preflight joined: {result:?}"
            ),
        }
        assert_eq!(
            preflight.await.expect("preflight task"),
            Err(SemanticActivationCoordinationErrorV1::Unavailable)
        );
        assert_eq!(
            control.execution_error(SemanticActivationCoordinationErrorV1::Unavailable),
            DaemonSemanticEvaluationExecutionErrorV1::TimedOut
        );
    }

    #[tokio::test]
    async fn request_cancellation_after_commit_preserves_publish_result() {
        let owner = Arc::new(DaemonSemanticEvaluationWorkerOwnerV1::default());
        let request_cancellation = CancellationToken::new();
        let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let execution = {
            let owner = Arc::clone(&owner);
            let request_cancellation = request_cancellation.clone();
            tokio::spawn(async move {
                owner
                    .execute(
                        tokio::time::Instant::now() + Duration::from_secs(5),
                        request_cancellation,
                        move |control| async move {
                            control.try_begin_commit()?;
                            let _ = commit_tx.send(());
                            let _ = release_rx.await;
                            Ok::<_, SemanticActivationCoordinationErrorV1>(())
                        },
                    )
                    .await
            })
        };
        commit_rx.await.expect("publish commit started");

        request_cancellation.cancel();
        release_tx.send(()).expect("release publish commit");

        assert_eq!(execution.await.expect("execution task"), Ok(()));
        let receipt = owner
            .cancel_and_join_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;
        assert!(receipt.is_clean());
        assert_eq!(receipt.remaining_workers, 0);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn checked_in_linux_quality_evaluation_records_process_resources() {
        let window = LinuxProcessResourceWindowV1::begin()
            .expect("Linux quality evaluation requires procfs and CLK_TCK");

        let (_cpu_time_us, peak_rss_bytes) = window
            .finish()
            .expect("Linux quality evaluation records CPU and peak RSS");

        assert!(peak_rss_bytes > 0);
    }
}
