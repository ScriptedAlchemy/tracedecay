use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracedecay_code_index::{
    graph_projection::{
        CODE_GRAPH_PROJECTOR_REVISION, code_graph_generation_id, code_graph_idempotency_key,
        code_graph_projection_identity,
    },
    production::CodeIndexPublishedGenerationV1,
};
use tracedecay_domain::{
    CodeGenerationId, ProjectId, RepositoryId, UtcMicros, VectorGenerationIdV1, WorktreeId,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig,
    GraphGenerationDependency, GraphProjectionIdentity, GraphProjectorRevision, GraphWriteBatch,
    NeverCancelled, VerifiedGenerationBatchCommit, VerifiedGraphSnapshot,
};
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter,
    exact_sql::{ExactSqlError, ExactSqlHandle, ExactSqlWriteAuthority, ExactSqlWriteIntent},
    reader::{ExactSqlOnlyReaderV1, ExistingReaderLocator, ReaderPool},
    repository::{
        ConcreteRepositoryWriteExecutor, GRAPH_PUBLICATION_SCHEMA_V1,
        SEMANTIC_VECTOR_STAGING_SCHEMA, SemanticVectorStagingExactSqlStorage,
    },
};
use tracedecay_store::{
    AdmissionConfigV1, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationInputDigestV1, GraphPublicationOperationContextV1, GraphPublicationStoreV1,
    GraphReplayAppendOutcomeV1, GraphVerifiedHeadV1, RetainedGraphStoreLeaseV1,
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    SemanticVectorPublishedGenerationKey, SemanticVectorPublishedGenerationLookup,
    SemanticVectorStageBatchReceipt, SemanticVectorStageCancelOutcome, SemanticVectorStageKey,
    SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageResumeOutcome, SemanticVectorStagingStore,
    StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1, canonical_store_locator_digest,
};

use crate::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1, SemanticVectorGraphScopeV1,
    VerifiedSemanticVectorGraphRuntimeV1,
};

mod support;

use support::{
    evaluation_binding, evaluation_source_namespace, evaluation_source_receipt_manifest,
    evaluation_source_scope, map_code_graph_error, map_publication_error, map_staging_error,
};

const POST_COMMIT_SETTLEMENT_DEADLINE: Duration = Duration::from_secs(30);
/// Isolated measurement graphs hash and settle a 10x corpus (~21700 × 768-d
/// pages). Production `GRAPH_OPERATION_DEADLINE` (30s) stays on the live
/// graph; this ceiling is eval-scoped and sized for that workload.
const EVALUATION_GRAPH_OPERATION_DEADLINE: Duration = Duration::from_secs(15 * 60);

fn evaluation_operation_deadline(requested: Instant) -> Instant {
    Instant::now()
        .checked_add(EVALUATION_GRAPH_OPERATION_DEADLINE)
        .map(|eval| eval.max(requested))
        .unwrap_or(requested)
}

#[derive(Debug)]
struct EvaluationGraphLeaseV1 {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
}

impl RetainedGraphStoreLeaseV1 for EvaluationGraphLeaseV1 {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

struct EvaluationSqlWriteAuthorityV1 {
    active: AtomicBool,
}

impl ExactSqlWriteAuthority for EvaluationSqlWriteAuthorityV1 {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if self.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(ExactSqlError::AuthorityDenied(
                "isolated semantic evaluation authority is closed".to_owned(),
            ))
        }
    }
}

struct EvaluationOperationProbeV1 {
    cancellation: Arc<dyn GraphCancellation>,
    deadline_at: Instant,
    cancellation_identity: RuntimeCancellationIdentityV1,
    deadline_identity: RuntimeDeadlineV1,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for EvaluationOperationProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation_identity
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline_identity
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        if self.cancellation.is_cancelled() {
            Some(RuntimeInterruptionV1::Cancelled)
        } else if Instant::now() >= self.deadline_at {
            Some(RuntimeInterruptionV1::DeadlineExceeded)
        } else {
            None
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

pub struct IsolatedSemanticEvaluationGraphV1 {
    registry: GraphDbRegistry,
    lease: Arc<EvaluationGraphLeaseV1>,
    binding: StoreRuntimeBindingV1,
    source_scope: StoreShardIdV1,
    project: ProjectId,
    repository: RepositoryId,
    worktree: WorktreeId,
    source_dependencies: BTreeMap<CodeGenerationId, GraphGenerationDependency>,
    cancellation: Arc<dyn GraphCancellation>,
    authority: Mutex<SemanticVectorStagingExactSqlStorage>,
    operation_sequence: AtomicU64,
    write_authority: Arc<EvaluationSqlWriteAuthorityV1>,
    _writer: Mutex<PersistentWriter>,
    _readers: ReaderPool<ExactSqlOnlyReaderV1>,
    _root: tempfile::TempDir,
}

struct IsolatedSemanticEvaluationRuntimeV1 {
    graph: Arc<IsolatedSemanticEvaluationGraphV1>,
    scope: SemanticVectorGraphScopeV1,
}

impl std::ops::Deref for IsolatedSemanticEvaluationRuntimeV1 {
    type Target = IsolatedSemanticEvaluationGraphV1;

    fn deref(&self) -> &Self::Target {
        self.graph.as_ref()
    }
}

impl Drop for IsolatedSemanticEvaluationGraphV1 {
    fn drop(&mut self) {
        self.write_authority.active.store(false, Ordering::Release);
    }
}

pub fn isolated_semantic_evaluation_graph(
    generations: &[&CodeIndexPublishedGenerationV1],
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Arc<IsolatedSemanticEvaluationGraphV1>, GraphDbError> {
    IsolatedSemanticEvaluationGraphV1::open(generations, cancellation).map(Arc::new)
}

impl IsolatedSemanticEvaluationGraphV1 {
    pub fn retained(
        self: &Arc<Self>,
        generation: &CodeGenerationId,
    ) -> Result<RetainedSemanticVectorGraphV1, GraphDbError> {
        let dependency = self.source_dependencies.get(generation).ok_or_else(|| {
            GraphDbError::invalid(
                "semantic evaluation requested a source generation outside its projected corpus",
            )
        })?;
        let code_scope_digest = canonical_sha256(&(
            "tracedecay.semantic-evaluation.code-scope.v1",
            &self.project,
            &self.repository,
            &self.worktree,
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let code_scope_hash = code_scope_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "semantic evaluation code-scope digest is not canonical".to_owned(),
            })
            .and_then(|hash| {
                tracedecay_store::SemanticVectorCodeScopeHash::new(hash)
                    .map_err(|error| GraphDbError::invalid(error.to_string()))
            })?;
        let scope = SemanticVectorGraphScopeV1::new(
            self.project.clone(),
            self.repository.clone(),
            self.worktree.clone(),
            generation.clone(),
            code_scope_hash,
            dependency.clone(),
        )?;
        let runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1> =
            Arc::new(IsolatedSemanticEvaluationRuntimeV1 {
                graph: Arc::clone(self),
                scope,
            });
        Ok(RetainedSemanticVectorGraphV1::new(
            runtime,
            Arc::clone(&self.cancellation),
        ))
    }
}

impl IsolatedSemanticEvaluationGraphV1 {
    fn open(
        generations: &[&CodeIndexPublishedGenerationV1],
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Self, GraphDbError> {
        if generations.is_empty() {
            return Err(GraphDbError::invalid(
                "semantic evaluation requires at least one projected code generation",
            ));
        }
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let root = tempfile::Builder::new()
            .prefix("tracedecay-semantic-evaluation-")
            .tempdir()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let canonical_root = root
            .path()
            .canonicalize()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let binding = evaluation_binding()?;
        let graph_path = canonical_root.join("evaluation.grafeo");
        let graph_locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(&graph_path)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        );
        let lease = Arc::new(EvaluationGraphLeaseV1 {
            binding: binding.clone(),
            locator: graph_locator,
            canonical_path: graph_path,
        });
        let metadata_path = canonical_root.join("evaluation.sqlite3");
        File::create(&metadata_path)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let metadata_path = metadata_path
            .canonicalize()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let metadata_locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(&metadata_path)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        );
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(
                binding.clone(),
                metadata_locator.clone(),
                metadata_path.clone(),
            )
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?,
            AdmissionConfigV1::default(),
            ConcreteRepositoryWriteExecutor::default(),
        )
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding.clone(), metadata_locator, metadata_path)
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?,
            AdmissionConfigV1::default().readers,
            ExactSqlOnlyReaderV1,
        )
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let handle = ExactSqlHandle::attach(&writer, &readers)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        handle
            .execute_batch(GRAPH_PUBLICATION_SCHEMA_V1.to_owned())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        handle
            .execute_batch(SEMANTIC_VECTOR_STAGING_SCHEMA.to_owned())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let write_authority = Arc::new(EvaluationSqlWriteAuthorityV1 {
            active: AtomicBool::new(true),
        });
        let handle = handle
            .with_write_authority(write_authority.clone())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let authority = SemanticVectorStagingExactSqlStorage::from_authorized_handle(handle)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let repository = RepositoryId::new("repository.semantic-evaluation")
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let worktree = WorktreeId::new("worktree.semantic-evaluation")
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let project = ProjectId::new("project.semantic-evaluation")
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let source_scope = evaluation_source_scope(&binding, &repository, &worktree)?;
        let runtime = Self {
            registry: GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 })?,
            lease,
            binding,
            source_scope,
            project,
            repository,
            worktree,
            source_dependencies: BTreeMap::new(),
            cancellation,
            authority: Mutex::new(authority),
            operation_sequence: AtomicU64::new(0),
            write_authority,
            _writer: Mutex::new(writer),
            _readers: readers,
            _root: root,
        };
        let mut runtime = runtime;
        for generation in generations {
            if runtime
                .source_dependencies
                .contains_key(&generation.manifest().generation_id)
            {
                continue;
            }
            let dependency = runtime.project_source_generation(generation)?;
            runtime
                .source_dependencies
                .insert(generation.manifest().generation_id.clone(), dependency);
        }
        Ok(runtime)
    }

    fn project_source_generation(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<GraphGenerationDependency, GraphDbError> {
        let check = || {
            if self.cancellation.is_cancelled() {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        let projector_revision =
            GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let source_generation_id = &generation.manifest().generation_id;
        let projection =
            code_graph_projection_identity(evaluation_source_namespace(source_generation_id)?)
                .map_err(map_code_graph_error)?;
        let graph_generation = code_graph_generation_id(source_generation_id, &projector_revision)
            .map_err(map_code_graph_error)?;
        let manifest = evaluation_source_receipt_manifest(
            projection.clone(),
            graph_generation.clone(),
            source_generation_id,
            &check,
        )?;
        let expected_recovered_digest = manifest.expected_recovered_digest(&check)?;
        let idempotency =
            code_graph_idempotency_key(&generation.manifest().generation_id, &projector_revision)
                .map_err(map_code_graph_error)?;
        let input_digest = GraphPublicationInputDigestV1::new(
            canonical_sha256(&(
                "tracedecay.semantic-evaluation-source-receipt.v1",
                &generation.manifest().generation_id,
                &manifest.generation,
                &manifest.source_generation,
                &manifest.watermark,
                &expected_recovered_digest,
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?
            .as_str(),
        )
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let relational_projection = GraphProjectionIdentityV1 {
            shard_id: self.binding.shard_id.clone(),
            namespace: GraphNamespaceV1::new(projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let replay = manifest.relational_replay(
            self.binding.shard_id.clone(),
            idempotency.clone(),
            input_digest,
            self.current_head(&relational_projection)?,
            &check,
        )?;
        let key = replay.key.clone();
        let cancellation = Arc::clone(&self.cancellation);
        let mut authority = self.authority()?;
        self.with_operation(
            Arc::clone(&cancellation),
            Instant::now() + std::time::Duration::from_secs(30),
            "source-append",
            |_, context| match authority
                .append_replay(&replay, context)
                .map_err(map_publication_error)?
            {
                GraphReplayAppendOutcomeV1::Appended(_)
                | GraphReplayAppendOutcomeV1::ExactReplay(_)
                | GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. } => Ok(()),
                outcome @ (GraphReplayAppendOutcomeV1::Conflict { .. }
                | GraphReplayAppendOutcomeV1::RetiredReplayConflict { .. }
                | GraphReplayAppendOutcomeV1::VerifiedHeadConflict { .. }
                | GraphReplayAppendOutcomeV1::PendingReplayConflict { .. }) => {
                    Err(GraphDbError::invalid(format!(
                        "semantic evaluation source {source_generation_id} append conflict: {outcome:?}"
                    )))
                }
            },
        )?;
        let snapshot = self.with_operation(
            cancellation,
            Instant::now() + std::time::Duration::from_secs(30),
            "source-publish",
            |registration, context| {
                self.registry
                    .publish_verified(registration, &mut *authority, context, &key, Some(manifest))
                    .map(|published| published.snapshot)
            },
        )?;
        if snapshot.verified_head().recovered_digest != expected_recovered_digest {
            return Err(GraphDbError::GenerationMismatch {
                namespace: projection.namespace.to_string(),
                projection: projection.projection.to_string(),
                generation: graph_generation.to_string(),
                message: "verified evaluation source receipt differs from its published identity"
                    .to_owned(),
            });
        }
        Ok(GraphGenerationDependency::new(
            projection,
            graph_generation,
            idempotency,
        ))
    }

    fn current_head(
        &self,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<Option<GraphVerifiedHeadV1>, GraphDbError> {
        let cancellation = Arc::clone(&self.cancellation);
        let mut authority = self.authority()?;
        self.with_operation(
            cancellation,
            Instant::now() + std::time::Duration::from_secs(30),
            "source-head",
            |_, context| {
                authority
                    .verified_head(projection, context)
                    .map_err(map_publication_error)
            },
        )
    }

    fn authority(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, SemanticVectorStagingExactSqlStorage>, GraphDbError> {
        self.authority.lock().map_err(|_| {
            GraphDbError::unavailable("semantic evaluation metadata authority lock is poisoned")
        })
    }

    fn registration(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> GraphDbRegistration {
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = self.lease.clone();
        GraphDbRegistration {
            authority_lease,
            lifecycle_cancellation: Arc::clone(&cancellation),
            cancellation,
            deadline,
        }
    }

    fn with_operation<T>(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
        label: &str,
        operation: impl FnOnce(
            GraphDbRegistration,
            &GraphPublicationOperationContextV1<'_>,
        ) -> Result<T, GraphDbError>,
    ) -> Result<T, GraphDbError> {
        let deadline = evaluation_operation_deadline(deadline);
        let sequence = self.operation_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let cancellation_identity = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "semantic-evaluation.{label}.{sequence}"
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            generation: 1,
        };
        let deadline_identity = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!(
                "semantic-evaluation.{label}.{sequence}"
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let requested_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))
            .and_then(|duration| {
                i64::try_from(duration.as_micros())
                    .map(UtcMicros)
                    .map_err(|error| GraphDbError::invalid(error.to_string()))
            })?;
        let control = RuntimeRequestControlV1 {
            requested_at,
            deadline: deadline_identity.clone(),
            cancellation: cancellation_identity.clone(),
        };
        let probe = EvaluationOperationProbeV1 {
            cancellation: Arc::clone(&cancellation),
            deadline_at: deadline,
            cancellation_identity,
            deadline_identity,
            commit_started: AtomicBool::new(false),
        };
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        operation(self.registration(cancellation, deadline), &context)
    }

    fn relational_projection(
        &self,
        projection: &GraphProjectionIdentity,
    ) -> Result<GraphProjectionIdentityV1, GraphDbError> {
        Ok(GraphProjectionIdentityV1 {
            shard_id: self.binding.shard_id.clone(),
            namespace: GraphNamespaceV1::new(projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        })
    }
}

impl VerifiedSemanticVectorGraphRuntimeV1 for IsolatedSemanticEvaluationRuntimeV1 {
    fn scope(&self) -> &SemanticVectorGraphScopeV1 {
        &self.scope
    }

    fn recover_verified_snapshot(
        &self,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        let projection = self.relational_projection(self.scope.projection())?;
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "recover-current",
            |registration, context| {
                if authority
                    .verified_head(&projection, context)
                    .map_err(map_publication_error)?
                    .is_none()
                {
                    return Ok(None);
                }
                self.registry
                    .recover_verified_snapshot(registration, &mut *authority, context, &projection)
                    .map(Some)
            },
        )
    }

    fn recover_verified_generation(
        &self,
        publication: &tracedecay_store::GraphPublicationKeyV1,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "recover-generation",
            |registration, context| {
                self.registry.verified_generation_snapshot(
                    registration,
                    &mut *authority,
                    context,
                    publication,
                )
            },
        )
    }

    fn staging_binding(&self) -> (&StoreShardIdV1, &StoreRuntimeBindingV1) {
        (&self.source_scope, &self.binding)
    }

    fn verified_head(
        &self,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<GraphVerifiedHeadV1>, GraphDbError> {
        let projection = self.relational_projection(self.scope.projection())?;
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "verified-head",
            |_, context| {
                authority
                    .verified_head(&projection, context)
                    .map_err(map_publication_error)
            },
        )
    }

    fn begin_stage(
        &self,
        plan: &SemanticVectorStagePlan,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageRecord, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-begin",
            |registration, context| {
                self.registry.begin_verified_generation(
                    registration,
                    &mut *authority,
                    context,
                    plan,
                )
            },
        )
    }

    fn resume_stage(
        &self,
        stage: &SemanticVectorStageKey,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageResumeOutcome, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-resume",
            |registration, context| {
                self.registry
                    .resume_generation_stage(registration, &mut *authority, context, stage)
            },
        )
    }

    fn published_semantic_generation(
        &self,
        key: &SemanticVectorPublishedGenerationKey,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorPublishedGenerationLookup, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "published-generation",
            |registration, context| {
                self.registry.published_semantic_generation(
                    registration,
                    &mut *authority,
                    context,
                    key,
                )
            },
        )
    }

    fn append_stage_batch(
        &self,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBatchCommit, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-receipt",
            |_, context| {
                let record = authority
                    .stage(&receipt.key.stage, context)
                    .map_err(map_staging_error)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "semantic evaluation stage is missing".to_owned(),
                    })?;
                match authority
                    .append_stage_batch(receipt, &record.plan.writer_fence, context)
                    .map_err(map_staging_error)?
                {
                    tracedecay_store::SemanticVectorStageAppendOutcome::Appended { .. }
                    | tracedecay_store::SemanticVectorStageAppendOutcome::ExactReplay { .. } => {
                        Ok(())
                    }
                    _ => Err(GraphDbError::Conflict),
                }
            },
        )?;
        let applied = self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-apply",
            |registration, context| {
                self.registry.apply_verified_generation_batch(
                    registration,
                    &mut *authority,
                    context,
                    &receipt.key,
                    &receipt.receipt_digest,
                    batch,
                )
            },
        )?;
        let settlement = SemanticGraphExecutionAuthorityV1::new(
            Arc::new(NeverCancelled),
            Instant::now() + POST_COMMIT_SETTLEMENT_DEADLINE,
        );
        let effect = self
            .with_operation(
                settlement.cancellation(),
                settlement.deadline(),
                "stage-settle-batch",
                |registration, context| {
                    self.registry.settle_verified_generation_batch(
                        registration,
                        &mut *authority,
                        context,
                        &receipt.key,
                        &receipt.receipt_digest,
                    )
                },
            )
            .map_err(post_commit_batch_settlement_error)?;
        Ok(VerifiedGenerationBatchCommit {
            commit: applied.commit,
            effect,
        })
    }

    fn cancel_stage(
        &self,
        stage: &SemanticVectorStageKey,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageCancelOutcome, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-cancel",
            |registration, context| {
                self.registry
                    .cancel_generation_stage(registration, &mut *authority, context, stage)
            },
        )
    }

    fn prepare_publication_from_staged_native(
        &self,
        stage: &SemanticVectorStageKey,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublicationPrepareOutcome, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-ready",
            |registration, context| {
                self.registry.prepare_publication_from_staged_native(
                    registration,
                    &mut *authority,
                    context,
                    stage,
                )
            },
        )
    }

    fn publish_ready_stage(
        &self,
        stage: &SemanticVectorStageKey,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-publish",
            |registration, context| {
                self.registry
                    .publish_ready_generation(registration, &mut *authority, context, stage)
                    .map(|commit| commit.snapshot)
            },
        )
    }

    fn settle_published(
        &self,
        settlement: &SemanticVectorStagePublishSettlement,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublishOutcome, GraphDbError> {
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "stage-settle",
            |_, context| {
                let record = authority
                    .stage(&settlement.stage, context)
                    .map_err(map_staging_error)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "published semantic evaluation stage is missing".to_owned(),
                    })?;
                authority
                    .settle_published(settlement, &record.plan.writer_fence, context)
                    .map_err(map_staging_error)
            },
        )
    }

    fn reserve_one_generation(
        &self,
        _after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionStep, GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not run retention",
        ))
    }

    fn finalize_reserved_generation(
        &self,
        _reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
        _authorization: &crate::semantic_runtime::SemanticVectorRetentionAuthorizationV1,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionAction, GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not run retention",
        ))
    }

    fn release_reserved_generation(
        &self,
        _reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
    ) -> Result<(), GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not run retention",
        ))
    }

    fn source_generation_has_live_reference(
        &self,
        _generation: &tracedecay_store::SemanticVectorSourceGenerationId,
        _expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not expose retention liveness",
        ))
    }

    fn source_scope_has_live_reference(
        &self,
        _source_scope: &StoreShardIdV1,
        _expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not expose retention liveness",
        ))
    }

    fn published_generation_dependency(
        &self,
        _generation: &VectorGenerationIdV1,
        _expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup, GraphDbError>
    {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not expose retention dependencies",
        ))
    }

    fn validate_project_census_revision(
        &self,
        _expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<(), GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not expose retention revisions",
        ))
    }

    fn source_scope_binding(
        &self,
        _code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        _expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorSourceScopeBindingLookup, GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not expose source-scope bindings",
        ))
    }

    fn remove_source_scope_binding(
        &self,
        _code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        _source_scope: &StoreShardIdV1,
        _expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        _authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        Err(GraphDbError::unavailable(
            "isolated semantic evaluation graphs do not mutate source-scope bindings",
        ))
    }
}

fn post_commit_batch_settlement_error(error: GraphDbError) -> GraphDbError {
    match error {
        GraphDbError::Cancelled | GraphDbError::DeadlineExceeded => {
            GraphDbError::DurabilityUncertain {
                message: "semantic evaluation batch was durably applied but stage settlement was interrupted; settlement remains replayable"
                    .to_owned(),
            }
        }
        error => error,
    }
}

#[cfg(test)]
mod settlement_tests {
    use super::*;

    #[test]
    fn post_commit_interruptions_are_durability_uncertain_not_cancelled() {
        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            assert!(matches!(
                post_commit_batch_settlement_error(interruption),
                GraphDbError::DurabilityUncertain { ref message }
                    if message.contains("settlement remains replayable")
            ));
        }
    }

    #[test]
    fn evaluation_source_namespaces_are_independent_roots() {
        let clean = CodeGenerationId::new("generation.evaluation-clean")
            .expect("clean evaluation source id");
        let one_symbol = CodeGenerationId::new("generation.evaluation-one-symbol")
            .expect("one-symbol evaluation source id");
        let clean_namespace =
            evaluation_source_namespace(&clean).expect("clean evaluation namespace");
        let one_symbol_namespace =
            evaluation_source_namespace(&one_symbol).expect("one-symbol evaluation namespace");
        assert_ne!(
            clean_namespace.as_str(),
            one_symbol_namespace.as_str(),
            "independent evaluation sources must not share a code-graph head"
        );
        assert!(
            clean_namespace
                .as_str()
                .contains("generation.evaluation-clean"),
            "namespace must bind the source generation: {}",
            clean_namespace.as_str()
        );
    }

    #[test]
    fn evaluation_source_receipt_is_identity_only_and_under_replay_bound() {
        let generation = CodeGenerationId::new("generation.evaluation-clean")
            .expect("clean evaluation source id");
        let projector_revision =
            GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
                .expect("code-graph projector revision");
        let projection = code_graph_projection_identity(
            evaluation_source_namespace(&generation).expect("evaluation namespace"),
        )
        .expect("evaluation source projection");
        let graph_generation = code_graph_generation_id(&generation, &projector_revision)
            .expect("evaluation source generation");
        let manifest =
            evaluation_source_receipt_manifest(projection, graph_generation, &generation, &|| {
                Ok(())
            })
            .expect("evaluation source receipt");
        assert!(
            manifest.entities.is_empty() && manifest.relations.is_empty(),
            "isolated evaluation must not inline the production code graph"
        );
        let replay_source = manifest
            .canonical_replay_source(&|| Ok(()))
            .expect("identity receipt replay");
        assert!(
            replay_source.len() < 64 * 1024,
            "identity receipt must stay far under the 4 MiB replay bound, got {} bytes",
            replay_source.len()
        );
    }

    #[test]
    fn evaluation_operation_deadline_is_eval_scoped_not_production_30s() {
        let requested = Instant::now() + Duration::from_secs(30);
        let eval = evaluation_operation_deadline(requested);
        assert!(
            eval.duration_since(Instant::now()) > Duration::from_secs(10 * 60),
            "isolated evaluation must keep a measurement-sized operation ceiling"
        );
        assert!(
            eval >= requested,
            "eval deadline must not shrink a longer caller deadline"
        );
    }
}
