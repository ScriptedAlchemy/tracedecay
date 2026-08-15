use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::CodeGenerationId;

use crate::config::retrieval::RetrievalRuntimeCompatibilityV1;
use crate::search_eval::semantic_native::{
    SemanticNativePendingReasonV1, SemanticNativeResourceProvenanceV1,
    SemanticNativeResourceSampleV1, SemanticNativeStageResultV1, SemanticProjectionCaseSampleV1,
    SemanticProjectionCaseV1,
};
use crate::search_eval::{
    CandidateOutputError, ProductionCandidateNativeExecutionAuthorityV1,
    ProductionCandidateNativeQueryContextV1, ProductionCandidateNativeQueryInputsV1,
    ProductionCandidateNativeResourceContextV1, evaluate_default_activation_candidate,
};
use tracedecay_usecases::semantic_runtime::{
    SemanticActivationCoordinationErrorV1, SemanticEvaluationAuthorityPublicationV1,
    SemanticEvaluationProfileCandidateV1, SemanticEvaluationPublicationSnapshotPortV1,
    SemanticEvaluationPublicationSnapshotV1, SemanticRuntimeFuture,
};

use super::code_index_scheduler::CodeIndexSchedulerRegistryV1;

static RESOURCE_MEASUREMENT_LOCK_V1: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

const EVALUATION_ACTIVE: u8 = 0;
const EVALUATION_CANCELLED: u8 = 1;
const EVALUATION_COMMIT_STARTED: u8 = 2;
const EVALUATION_TIMED_OUT: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DaemonSemanticEvaluationExecutionErrorV1 {
    Cancelled,
    TimedOut,
    Coordination(SemanticActivationCoordinationErrorV1),
}

pub(crate) struct DaemonSemanticEvaluationControlV1 {
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
        match self.phase.load(Ordering::Acquire) {
            EVALUATION_CANCELLED => DaemonSemanticEvaluationExecutionErrorV1::Cancelled,
            EVALUATION_TIMED_OUT => DaemonSemanticEvaluationExecutionErrorV1::TimedOut,
            _ => DaemonSemanticEvaluationExecutionErrorV1::Coordination(fallback),
        }
    }

    fn checkpoint(&self) -> Result<(), SemanticActivationCoordinationErrorV1> {
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

    async fn interruptible<Output>(
        &self,
        operation: impl Future<Output = Output>,
    ) -> Result<Output, SemanticActivationCoordinationErrorV1> {
        self.checkpoint()?;
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SemanticEvaluationShutdownReceiptV1 {
    pub(crate) joined_workers: usize,
    /// Workers whose join surfaced a panic or abort instead of a cooperative
    /// exit. They are no longer running but did not shut down cleanly.
    pub(crate) failed_workers: usize,
    pub(crate) remaining_workers: usize,
}

impl SemanticEvaluationShutdownReceiptV1 {
    pub(crate) fn is_clean(self) -> bool {
        self.remaining_workers == 0 && self.failed_workers == 0
    }
}

#[derive(Default)]
pub(crate) struct DaemonSemanticEvaluationWorkerOwnerV1 {
    workers: Mutex<SemanticEvaluationWorkersV1>,
}

impl DaemonSemanticEvaluationWorkerOwnerV1 {
    pub(crate) async fn execute<Output, Work, WorkFuture>(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
        work: Work,
    ) -> Result<Output, DaemonSemanticEvaluationExecutionErrorV1>
    where
        Output: Send + 'static,
        Work: FnOnce(Arc<DaemonSemanticEvaluationControlV1>) -> WorkFuture + Send + 'static,
        WorkFuture:
            Future<Output = Result<Output, SemanticActivationCoordinationErrorV1>> + Send + 'static,
    {
        let cancellation = CancellationToken::new();
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
            let handle = tokio::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                let mut evaluation = Box::pin(work(Arc::clone(&worker_control)));
                let outcome = tokio::select! {
                    result = &mut evaluation => {
                        result.map_err(|error| worker_control.execution_error(error))
                    }
                    () = worker_control.cancellation.cancelled() => {
                        let _ = evaluation.await;
                        Err(worker_control.execution_error(
                            SemanticActivationCoordinationErrorV1::Unavailable,
                        ))
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

    pub(crate) async fn cancel_and_join_until(
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

#[derive(Clone)]
pub(super) struct DaemonSemanticEvaluationSnapshotAuthorityV1 {
    project_root: PathBuf,
    scope: ResolvedScope,
    scheduler: CodeIndexSchedulerRegistryV1,
    candidate: SemanticEvaluationProfileCandidateV1,
    control: Arc<DaemonSemanticEvaluationControlV1>,
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
}

impl DaemonSemanticEvaluationSnapshotAuthorityV1 {
    pub(super) fn new(
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
            prepared_native: Arc::new(Mutex::new(BTreeMap::new())),
            projection_cases: Arc::new(Mutex::new(BTreeMap::new())),
        }
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
            let generation = runtime
                .prepare_evaluation_generation(
                    context.code,
                    Arc::clone(&self.control)
                        as Arc<dyn tracedecay_semantic::SemanticEvaluationCancellationV1>,
                )
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
        let result = generation.with_query_inputs(context, rerank_authority.as_ref(), evaluate);
        self.control.checkpoint().map_err(|_| {
            CandidateOutputError::Contract("semantic evaluation was cancelled".to_owned())
        })?;
        result
    }

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
                let generation = runtime
                    .prepare_evaluation_generation(
                        context.code,
                        Arc::clone(&self.control)
                            as Arc<dyn tracedecay_semantic::SemanticEvaluationCancellationV1>,
                    )
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
        let latency_samples_us = execute_queries()?;
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
            let mut resources = runtime
                .measure_incremental_evaluation_projection(prepared, context.incremental_code)
                .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
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
                    let measured = runtime
                        .measure_evaluation_projection_cases(
                            prepared,
                            &context.semantic_projection_sources,
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
        if resources.source_generation != *context.code_generation
            || resources.source_manifest_digest
                != context.code.projection().request().changes.manifest_digest
            || resources.incremental_source_generation
                != context.incremental_code.manifest().generation_id
            || resources.incremental_source_manifest_digest
                != context
                    .incremental_code
                    .projection()
                    .request()
                    .changes
                    .manifest_digest
            || (semantic_resources.is_some()
                && (resources.model_bytes == 0
                    || resources.tokenizer_bytes == 0
                    || resources.threads == 0
                    || resources.batch_size == 0
                    || resources.sequence_length == 0
                    || resources.load_deadline_ms == 0
                    || resources.cold_model_load_micros == 0
                    || resources.vector_bytes == 0
                    || resources.projection_cases.len() != 7))
        {
            return Err(CandidateOutputError::Contract(
                "semantic resource measurement is not bound to the exact prepared generation"
                    .to_owned(),
            ));
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

impl SemanticEvaluationPublicationSnapshotPortV1 for DaemonSemanticEvaluationSnapshotAuthorityV1 {
    fn current(
        &self,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<SemanticEvaluationPublicationSnapshotV1, SemanticActivationCoordinationErrorV1>,
    > {
        Box::pin(async move {
            self.control.checkpoint()?;
            let code = self
                .control
                .interruptible(
                    self.scheduler
                        .semantic_evaluation_snapshot_for_scope(&self.scope),
                )
                .await?
                .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
            let (
                semantic_source_generation,
                semantic_ceiling,
                vector_state_revision,
                vector_generation_id,
            ) = match self.candidate.compatibility.semantic.as_ref() {
                Some(required) => {
                    let runtime =
                        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                            &self.project_root,
                        )
                        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
                    let semantic = self
                        .control
                        .interruptible(runtime.inspect_evaluation_current_generation_snapshot(
                            required,
                            &code.source_generation,
                            &code.source_manifest_digest,
                        ))
                        .await?
                        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
                    (
                        Some(code.source_generation.clone()),
                        None,
                        Some(semantic.vector_state_revision),
                        Some(semantic.vector_generation_id),
                    )
                }
                None => (None, None, None, None),
            };
            let evaluated = crate::search_eval::load_default_evaluated_profile_material(
                &self.candidate.evaluated_profile_id,
            )
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
            self.control.checkpoint()?;
            Ok(SemanticEvaluationPublicationSnapshotV1 {
                project_root: self.project_root.clone(),
                scope: self.scope.clone(),
                code_generation: code.source_generation,
                code_source_manifest_digest: code.source_manifest_digest,
                code_snapshot_digest: code.snapshot_digest,
                semantic_source_generation,
                vector_state_revision,
                vector_generation_id,
                runtime: RetrievalRuntimeCompatibilityV1 {
                    retrieval_ceiling:
                        super::code_index_scheduler::queries::maximum_retrieval_budget(),
                    semantic: self.candidate.compatibility.semantic.clone(),
                    semantic_ceiling,
                    rerank: self.candidate.compatibility.rerank.clone(),
                    rerank_ceiling: evaluated.rerank,
                },
            })
        })
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
            let result = tokio::task::spawn_blocking(move || {
                let _measurement = measurement;
                authority.control.checkpoint()?;
                evaluate_default_activation_candidate(&evaluated_profile_id, &authority).map_err(
                    |error| {
                        SemanticActivationCoordinationErrorV1::RejectedDetail(error.to_string())
                    },
                )
            })
            .await
            .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
            self.control.checkpoint()?;
            result
        })
    }

    fn publish_if_current<'a>(
        &'a self,
        expected: &'a SemanticEvaluationPublicationSnapshotV1,
        publication: SemanticEvaluationAuthorityPublicationV1,
    ) -> SemanticRuntimeFuture<'a, Result<(), SemanticActivationCoordinationErrorV1>> {
        Box::pin(async move {
            self.control.checkpoint()?;
            let expected_code = super::code_index_scheduler::SemanticEvaluationCodeSnapshotV1 {
                source_generation: expected.code_generation.clone(),
                source_manifest_digest: expected.code_source_manifest_digest.clone(),
                snapshot_digest: expected.code_snapshot_digest.clone(),
            };
            let _code_lease = self
                .control
                .interruptible(
                    self.scheduler
                        .acquire_semantic_evaluation_publication_lease(&self.scope, &expected_code),
                )
                .await?
                .ok_or(SemanticActivationCoordinationErrorV1::Conflict)?;
            let runtime = match (
                self.candidate.compatibility.semantic.as_ref(),
                expected.vector_state_revision,
                expected.vector_generation_id.as_ref(),
            ) {
                (Some(_), Some(revision), Some(generation)) => Some((
                    tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(
                        &self.project_root,
                    )
                    .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?,
                    revision,
                    generation,
                )),
                (None, None, None) => None,
                _ => return Err(SemanticActivationCoordinationErrorV1::Rejected),
            };
            let _vector_lease = match runtime.as_ref() {
                Some((runtime, revision, generation)) => Some(
                    self.control
                        .interruptible(
                            runtime.acquire_vector_publication_lease(*revision, generation),
                        )
                        .await?
                        .map_err(|_| SemanticActivationCoordinationErrorV1::Conflict)?,
                ),
                None => None,
            };
            if let (Some((runtime, revision, generation)), Some(required)) =
                (runtime.as_ref(), publication.semantic_compatibility())
            {
                let observed = self
                    .control
                    .interruptible(runtime.inspect_compatible_current_generation_snapshot(
                        required,
                        &expected.code_generation,
                        &expected.code_source_manifest_digest,
                    ))
                    .await?
                    .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
                if observed.vector_state_revision != *revision
                    || &observed.vector_generation_id != *generation
                {
                    return Err(SemanticActivationCoordinationErrorV1::Conflict);
                }
            }
            self.control.try_begin_commit()?;
            let result = publication.commit(expected).await;
            self.control.checkpoint()?;
            result
        })
    }
}

struct LinuxProcessResourceWindowV1 {
    cpu_ticks: u64,
    ticks_per_second: u64,
}

impl LinuxProcessResourceWindowV1 {
    #[cfg(target_os = "linux")]
    fn begin() -> Option<Self> {
        let output = std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let ticks_per_second = std::str::from_utf8(&output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|ticks| *ticks != 0)?;
        Some(Self {
            cpu_ticks: read_linux_process_cpu_ticks()?,
            ticks_per_second,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn begin() -> Option<Self> {
        None
    }

    fn finish(self) -> Option<(u64, u64)> {
        let elapsed_ticks = read_linux_process_cpu_ticks()?.saturating_sub(self.cpu_ticks);
        let cpu_time_us = u64::try_from(
            u128::from(elapsed_ticks)
                .checked_mul(1_000_000)?
                .checked_div(u128::from(self.ticks_per_second))?,
        )
        .ok()?;
        Some((cpu_time_us, read_linux_process_lifetime_peak_rss_bytes()?))
    }
}

#[cfg(target_os = "linux")]
fn read_linux_process_cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let fields = fields.collect::<Vec<_>>();
    let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
    user_ticks.checked_add(system_ticks)
}

#[cfg(not(target_os = "linux"))]
fn read_linux_process_cpu_ticks() -> Option<u64> {
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
}

#[cfg(not(target_os = "linux"))]
fn read_linux_process_lifetime_peak_rss_bytes() -> Option<u64> {
    None
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
