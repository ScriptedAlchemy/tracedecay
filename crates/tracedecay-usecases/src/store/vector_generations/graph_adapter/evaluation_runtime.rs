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
    GraphCancellation, GraphDbError, GraphDbOwnerAttachmentV1, GraphDbOwnerRegistrationV1,
    GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphDbRetirementOutcome,
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
    GraphPublicationInputDigestV1, GraphPublicationOperationContextV1,
    GraphPublicationReplayLookupV1, GraphPublicationStoreV1, GraphReplayAppendOutcomeV1,
    GraphVerifiedHeadV1, RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
    RetainedGraphStoreOwnerOperationLeaseErrorV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1, SemanticVectorPublicationAuthority,
    SemanticVectorPublishedGenerationKey, SemanticVectorPublishedGenerationLookup,
    SemanticVectorStageBatchReceipt, SemanticVectorStageBeginOutcome,
    SemanticVectorStageCancelOutcome, SemanticVectorStageIncomplete, SemanticVectorStageKey,
    SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageResumeOutcome, SemanticVectorStageState,
    SemanticVectorStagingStore, StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
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

#[derive(Debug)]
struct EvaluationGraphOwnerAttachmentV1 {
    operation: Arc<EvaluationGraphLeaseV1>,
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

impl RetainedGraphStoreOwnerAttachmentV1 for EvaluationGraphOwnerAttachmentV1 {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.operation.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.operation.locator
    }

    fn canonical_path(&self) -> &Path {
        &self.operation.canonical_path
    }

    fn issue_operation_lease(
        &self,
    ) -> Result<Arc<dyn RetainedGraphStoreLeaseV1>, RetainedGraphStoreOwnerOperationLeaseErrorV1>
    {
        Ok(self.operation.clone())
    }
}

struct EvaluationSqlWriteAuthorityV1 {
    active: AtomicBool,
}

impl EvaluationSqlWriteAuthorityV1 {
    fn close(&self) {
        self.active.store(false, Ordering::Release);
    }
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
    graph_owner: Mutex<Option<GraphDbOwnerAttachmentV1>>,
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
    _root: Option<tempfile::TempDir>,
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
        self.write_authority.close();
        // Close the native owner while the tempdir still exists. Field drop
        // order would otherwise unlink `_root` under a live registry entry.
        // If retirement fails, leak the directory rather than unlinking it
        // under a still-mounted owner.
        let retired_ok = match self.graph_owner.lock() {
            Ok(mut owner) => match owner.take() {
                Some(owner) => retire_evaluation_graph_runtime(&self.registry, &owner).is_ok(),
                None => true,
            },
            Err(_) => false,
        };
        dispose_evaluation_root(self._root.take(), retired_ok);
    }
}

pub fn isolated_semantic_evaluation_graph(
    generations: &[&CodeIndexPublishedGenerationV1],
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Arc<IsolatedSemanticEvaluationGraphV1>, GraphDbError> {
    IsolatedSemanticEvaluationGraphV1::open(generations, cancellation).map(Arc::new)
}

impl IsolatedSemanticEvaluationGraphV1 {
    pub fn retire(&self) -> Result<(), GraphDbError> {
        let owner = self
            .graph_owner
            .lock()
            .map_err(|_| {
                GraphDbError::unavailable("semantic evaluation graph owner lock is poisoned")
            })?
            .take();
        let Some(owner) = owner else {
            self.write_authority.close();
            return Ok(());
        };
        match retire_evaluation_graph_runtime(&self.registry, &owner) {
            Ok(_) => {
                self.write_authority.close();
                Ok(())
            }
            Err(error) => {
                if let Ok(mut slot) = self.graph_owner.lock() {
                    *slot = Some(owner);
                }
                Err(error)
            }
        }
    }

    pub fn retained(
        self: &Arc<Self>,
        generation: &CodeGenerationId,
    ) -> Result<RetainedSemanticVectorGraphV1, GraphDbError> {
        self.require_mounted_owner()?;
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

    fn require_mounted_owner(&self) -> Result<(), GraphDbError> {
        require_mounted_evaluation_owner(&self.graph_owner)
    }
}

fn mount_evaluation_graph_runtime(
    registry: &GraphDbRegistry,
    operation: Arc<EvaluationGraphLeaseV1>,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<GraphDbOwnerAttachmentV1, GraphDbError> {
    let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = operation.clone();
    registry.resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
        operation: GraphDbRegistration {
            authority_lease,
            lifecycle_cancellation: Arc::clone(&cancellation),
            cancellation,
            deadline: Instant::now() + EVALUATION_GRAPH_OPERATION_DEADLINE,
        },
        authority_attachment: Box::new(EvaluationGraphOwnerAttachmentV1 { operation }),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvaluationRootDisposal {
    Released,
    LeakedUntilProcessExit,
}

fn dispose_evaluation_root(
    root: Option<tempfile::TempDir>,
    retired_ok: bool,
) -> EvaluationRootDisposal {
    match root {
        Some(root) if !retired_ok => {
            std::mem::forget(root);
            EvaluationRootDisposal::LeakedUntilProcessExit
        }
        Some(_) | None => EvaluationRootDisposal::Released,
    }
}

fn require_mounted_evaluation_owner(
    graph_owner: &Mutex<Option<GraphDbOwnerAttachmentV1>>,
) -> Result<(), GraphDbError> {
    let owner = graph_owner.lock().map_err(|_| {
        GraphDbError::unavailable("semantic evaluation graph owner lock is poisoned")
    })?;
    if owner.is_none() {
        return Err(GraphDbError::unavailable(
            "semantic evaluation graph owner has been retired",
        ));
    }
    Ok(())
}

fn retire_evaluation_graph_runtime(
    registry: &GraphDbRegistry,
    owner: &GraphDbOwnerAttachmentV1,
) -> Result<GraphDbRetirementOutcome, GraphDbError> {
    let mut reservation = registry
        .reserve_retirement_batch(vec![owner.retirement_target()])
        .map_err(|refusal| refusal.into_parts().0)?;
    let commit = reservation
        .commit(
            Arc::new(NeverCancelled),
            Instant::now() + Duration::from_secs(30),
        )
        .map_err(|refusal| refusal.into_parts().0)?;
    match commit.outcomes() {
        [outcome] => match outcome {
            GraphDbRetirementOutcome::Closed(_) => Ok(outcome.clone()),
            GraphDbRetirementOutcome::DurabilityUncertain { message, .. } => {
                Err(GraphDbError::DurabilityUncertain {
                    message: message.clone(),
                })
            }
            GraphDbRetirementOutcome::Failed { error, .. } => Err(error.clone()),
        },
        outcomes => Err(GraphDbError::unavailable(format!(
            "evaluation graph retirement expected one outcome, got {}",
            outcomes.len()
        ))),
    }
}

fn isolated_stage_reuses_the_same_semantic_generation(
    existing: &SemanticVectorStagePlan,
    requested: &SemanticVectorStagePlan,
) -> bool {
    existing.key.projection == requested.key.projection
        && existing.semantic_generation_id == requested.semantic_generation_id
        && existing.base_generation == requested.base_generation
        && existing.source_scope == requested.source_scope
        && existing.source_generation == requested.source_generation
        && existing.source_dependency == requested.source_dependency
        && existing.recipe == requested.recipe
        && existing.expected_chunk_count == requested.expected_chunk_count
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IsolatedNativePrepareGate {
    Conflict,
    Cancelled,
    ExactReplay,
    Incomplete,
}

fn isolated_native_prepare_gate(
    record_key_matches: bool,
    state: SemanticVectorStageState,
) -> IsolatedNativePrepareGate {
    if !record_key_matches || state == SemanticVectorStageState::Published {
        IsolatedNativePrepareGate::Conflict
    } else if state == SemanticVectorStageState::Cancelled {
        IsolatedNativePrepareGate::Cancelled
    } else if state == SemanticVectorStageState::ReadyToPublish {
        IsolatedNativePrepareGate::ExactReplay
    } else {
        IsolatedNativePrepareGate::Incomplete
    }
}

fn isolated_pending_prepare_incomplete(
    record: &SemanticVectorStageRecord,
) -> SemanticVectorStageIncomplete {
    SemanticVectorStageIncomplete {
        expected_chunks: record.plan.expected_chunk_count,
        recorded_chunks: record.recorded_chunk_count,
        pending_batches: record
            .plan
            .expected_chunk_count
            .saturating_sub(record.recorded_chunk_count),
        failed_batches: 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IsolatedNativePublishGate {
    Conflict,
    IsolatedLocal,
}

fn isolated_native_publish_gate(
    record_key_matches: bool,
    state: SemanticVectorStageState,
) -> IsolatedNativePublishGate {
    if !record_key_matches {
        IsolatedNativePublishGate::Conflict
    } else if state == SemanticVectorStageState::Published
        || state == SemanticVectorStageState::ReadyToPublish
    {
        IsolatedNativePublishGate::IsolatedLocal
    } else {
        IsolatedNativePublishGate::Conflict
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IsolatedNativeSettleGate {
    Conflict,
    IsolatedLocal,
}

fn isolated_native_settle_gate(state: SemanticVectorStageState) -> IsolatedNativeSettleGate {
    if state == SemanticVectorStageState::Cancelled || state == SemanticVectorStageState::Pending {
        IsolatedNativeSettleGate::Conflict
    } else {
        IsolatedNativeSettleGate::IsolatedLocal
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IsolatedNativeRecoverGate {
    Conflict,
    IsolatedLocal,
}

fn isolated_native_recover_publication_gate(projection_matches: bool) -> IsolatedNativeRecoverGate {
    if projection_matches {
        IsolatedNativeRecoverGate::IsolatedLocal
    } else {
        IsolatedNativeRecoverGate::Conflict
    }
}

fn isolated_pending_stage_cancel_is_blocked(
    replay: &GraphPublicationReplayLookupV1,
    verified_head_matches_publication: bool,
) -> bool {
    !matches!(replay, GraphPublicationReplayLookupV1::Missing) || verified_head_matches_publication
}

fn require_evaluation_recovery_replay(
    lookup: GraphPublicationReplayLookupV1,
) -> Result<(), GraphDbError> {
    match lookup {
        GraphPublicationReplayLookupV1::Active(_) => Ok(()),
        GraphPublicationReplayLookupV1::Retired(_) => Err(GraphDbError::Conflict),
        GraphPublicationReplayLookupV1::Missing => Err(GraphDbError::Corrupt {
            message: "exact verified graph generation has no durable active replay".to_owned(),
        }),
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
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 })?;
        let graph_owner = mount_evaluation_graph_runtime(
            &registry,
            Arc::clone(&lease),
            Arc::clone(&cancellation),
        )?;
        let runtime = Self {
            registry,
            graph_owner: Mutex::new(Some(graph_owner)),
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
            _root: Some(root),
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
                    .publish_verified(
                        registration,
                        &mut *authority,
                        context,
                        &key,
                        Some(Arc::new(manifest)),
                    )
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
        self.require_mounted_owner()?;
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
                let Some(head) = authority
                    .verified_head(&projection, context)
                    .map_err(map_publication_error)?
                else {
                    return Ok(None);
                };
                match isolated_native_recover_publication_gate(head.key.projection == projection) {
                    IsolatedNativeRecoverGate::Conflict => return Err(GraphDbError::Conflict),
                    IsolatedNativeRecoverGate::IsolatedLocal => {}
                }
                require_evaluation_recovery_replay(
                    authority
                        .replay(&head.key, context)
                        .map_err(map_publication_error)?,
                )?;
                // Isolated recover must bind the Isolated-local replay, not
                // reinstall a sealed generation into Grafeo. That replay is
                // what made production graph.activate take ~10 minutes.
                self.registry
                    .verified_generation_snapshot(registration, &mut *authority, context, &head.key)
                    .map(Some)
            },
        )
    }

    fn recover_verified_generation(
        &self,
        publication: &tracedecay_store::GraphPublicationKeyV1,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let expected = self.relational_projection(self.scope.projection())?;
        let mut authority = self.authority()?;
        self.with_operation(
            execution.cancellation(),
            execution.deadline(),
            "recover-generation",
            |registration, context| {
                match isolated_native_recover_publication_gate(publication.projection == expected) {
                    IsolatedNativeRecoverGate::Conflict => return Err(GraphDbError::Conflict),
                    IsolatedNativeRecoverGate::IsolatedLocal => {}
                }
                require_evaluation_recovery_replay(
                    authority
                        .replay(publication, context)
                        .map_err(map_publication_error)?,
                )?;
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
            |_registration, context| {
                if plan.key.projection.shard_id != self.binding.shard_id
                    || plan.publication_key.projection.shard_id != self.binding.shard_id
                {
                    return Err(GraphDbError::Conflict);
                }
                plan.validate()
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
                match authority
                    .begin_stage(plan, context)
                    .map_err(map_staging_error)?
                {
                    SemanticVectorStageBeginOutcome::Begun(record)
                    | SemanticVectorStageBeginOutcome::ExactReplay(record) => {
                        if record.plan != *plan {
                            return Err(GraphDbError::Conflict);
                        }
                        Ok(record)
                    }
                    SemanticVectorStageBeginOutcome::Published { record, .. } => {
                        if !isolated_stage_reuses_the_same_semantic_generation(&record.plan, plan) {
                            return Err(GraphDbError::Conflict);
                        }
                        Ok(*record)
                    }
                    SemanticVectorStageBeginOutcome::InputConflict { .. }
                    | SemanticVectorStageBeginOutcome::SemanticGenerationConflict { .. }
                    | SemanticVectorStageBeginOutcome::PublicationConflict
                    | SemanticVectorStageBeginOutcome::PriorVerifiedHeadConflict { .. } => {
                        Err(GraphDbError::Conflict)
                    }
                }
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
            |_registration, context| {
                let Some(record) = authority.stage(stage, context).map_err(map_staging_error)?
                else {
                    return Ok(SemanticVectorStageResumeOutcome::Missing);
                };
                if record.plan.key != *stage {
                    return Err(GraphDbError::Conflict);
                }
                match record.state {
                    SemanticVectorStageState::Published => {
                        let key = SemanticVectorPublishedGenerationKey {
                            projection: record.plan.key.projection.clone(),
                            semantic_generation_id: record.plan.semantic_generation_id.clone(),
                        };
                        match authority
                            .published_semantic_generation(&key, context)
                            .map_err(map_staging_error)?
                        {
                            SemanticVectorPublishedGenerationLookup::Published {
                                record: verified_record,
                                verified_head,
                            } if *verified_record == record => {
                                Ok(SemanticVectorStageResumeOutcome::Published {
                                    record: verified_record,
                                    verified_head,
                                })
                            }
                            SemanticVectorPublishedGenerationLookup::Published { .. }
                            | SemanticVectorPublishedGenerationLookup::Missing => {
                                Err(GraphDbError::Conflict)
                            }
                        }
                    }
                    SemanticVectorStageState::Cancelled => {
                        Ok(SemanticVectorStageResumeOutcome::Cancelled(record))
                    }
                    SemanticVectorStageState::Pending
                    | SemanticVectorStageState::ReadyToPublish => {
                        let pending = authority
                            .pending_stage(&stage.projection, context)
                            .map_err(map_staging_error)?;
                        if pending.as_ref() != Some(&record) {
                            return Err(GraphDbError::Conflict);
                        }
                        match record.state {
                            SemanticVectorStageState::Pending => {
                                Ok(SemanticVectorStageResumeOutcome::Pending(record))
                            }
                            SemanticVectorStageState::ReadyToPublish => {
                                require_evaluation_recovery_replay(
                                    authority
                                        .replay(&record.plan.publication_key, context)
                                        .map_err(map_publication_error)?,
                                )?;
                                Ok(SemanticVectorStageResumeOutcome::Ready(record))
                            }
                            SemanticVectorStageState::Published
                            | SemanticVectorStageState::Cancelled => Err(GraphDbError::Corrupt {
                                message: "terminal semantic vector stage changed during resume"
                                    .to_owned(),
                            }),
                        }
                    }
                }
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
            |_registration, context| {
                if self.binding.shard_id != key.projection.shard_id {
                    return Err(GraphDbError::Conflict);
                }
                authority
                    .published_semantic_generation(key, context)
                    .map_err(map_staging_error)
            },
        )
    }

    fn append_stage_batch(
        &self,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        execution: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBatchCommit, GraphDbError> {
        let expected = self.relational_projection(self.scope.projection())?;
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
                let record = authority
                    .stage(&receipt.key.stage, context)
                    .map_err(map_staging_error)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "semantic evaluation stage is missing".to_owned(),
                    })?;
                match isolated_native_recover_publication_gate(
                    record.plan.key.projection == expected,
                ) {
                    IsolatedNativeRecoverGate::Conflict => return Err(GraphDbError::Conflict),
                    IsolatedNativeRecoverGate::IsolatedLocal => {}
                }
                if record.state == SemanticVectorStageState::Cancelled {
                    return Err(GraphDbError::Conflict);
                }
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
                    let record = authority
                        .stage(&receipt.key.stage, context)
                        .map_err(map_staging_error)?
                        .ok_or_else(|| GraphDbError::ResetRequired {
                            message: "semantic evaluation stage is missing".to_owned(),
                        })?;
                    match isolated_native_settle_gate(record.state) {
                        IsolatedNativeSettleGate::Conflict => return Err(GraphDbError::Conflict),
                        IsolatedNativeSettleGate::IsolatedLocal => {}
                    }
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
            |_registration, context| {
                // This graph and its native rows share one temporary owner and
                // are destroyed together after evaluation. Terminalize the
                // canonical staging authority here, but do not run the
                // persistent registry's page-wise native retirement while the
                // evaluator is still measuring projection behavior.
                let Some(record) = authority.stage(stage, context).map_err(map_staging_error)?
                else {
                    return Ok(SemanticVectorStageCancelOutcome::MissingStage);
                };
                if record.plan.key != *stage {
                    return Err(GraphDbError::Conflict);
                }
                if record.state == SemanticVectorStageState::Cancelled {
                    return Ok(SemanticVectorStageCancelOutcome::ExactReplay(record));
                }
                if record.state != SemanticVectorStageState::Pending {
                    return Ok(SemanticVectorStageCancelOutcome::ReadyToPublish(record));
                }
                let replay = authority
                    .replay(&record.plan.publication_key, context)
                    .map_err(map_publication_error)?;
                let head = authority
                    .verified_head(&record.plan.key.projection, context)
                    .map_err(map_publication_error)?;
                if isolated_pending_stage_cancel_is_blocked(
                    &replay,
                    head.as_ref()
                        .is_some_and(|head| head.key == record.plan.publication_key),
                ) {
                    return Err(GraphDbError::Conflict);
                }
                authority
                    .cancel_stage(stage, &record.plan.writer_fence, context)
                    .map_err(map_staging_error)
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
            |_registration, context| {
                let Some(record) = authority.stage(stage, context).map_err(map_staging_error)?
                else {
                    return Err(GraphDbError::ResetRequired {
                        message: "semantic vector stage is missing before Isolated prepare"
                            .to_owned(),
                    });
                };
                match isolated_native_prepare_gate(record.plan.key == *stage, record.state) {
                    IsolatedNativePrepareGate::Conflict => Err(GraphDbError::Conflict),
                    IsolatedNativePrepareGate::Cancelled => Ok(
                        SemanticVectorStagePublicationPrepareOutcome::Cancelled(record),
                    ),
                    IsolatedNativePrepareGate::ExactReplay => Ok(
                        SemanticVectorStagePublicationPrepareOutcome::ExactReplay(record),
                    ),
                    IsolatedNativePrepareGate::Incomplete => {
                        Ok(SemanticVectorStagePublicationPrepareOutcome::Incomplete(
                            isolated_pending_prepare_incomplete(&record),
                        ))
                    }
                }
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
                let record = authority
                    .stage(stage, context)
                    .map_err(map_staging_error)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "ready semantic vector stage is missing".to_owned(),
                    })?;
                match isolated_native_publish_gate(record.plan.key == *stage, record.state) {
                    IsolatedNativePublishGate::Conflict => Err(GraphDbError::Conflict),
                    IsolatedNativePublishGate::IsolatedLocal => {
                        require_evaluation_recovery_replay(
                            authority
                                .replay(&record.plan.publication_key, context)
                                .map_err(map_publication_error)?,
                        )?;
                        // Isolated publish binds Isolated-local replay. It
                        // must not run native Grafeo publish/install — that
                        // replay path is what made production activate take
                        // ~10 minutes and what CompactStore is replacing.
                        self.registry.verified_generation_snapshot(
                            registration,
                            &mut *authority,
                            context,
                            &record.plan.publication_key,
                        )
                    }
                }
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
                match isolated_native_settle_gate(record.state) {
                    IsolatedNativeSettleGate::Conflict => return Err(GraphDbError::Conflict),
                    IsolatedNativeSettleGate::IsolatedLocal => {}
                }
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
    fn evaluation_graph_mounts_owner_before_registered_operations() {
        let root = tempfile::tempdir().expect("evaluation root");
        let graph_path = root
            .path()
            .canonicalize()
            .expect("canonical evaluation root")
            .join("evaluation.grafeo");
        let binding = evaluation_binding().expect("evaluation binding");
        let operation = Arc::new(EvaluationGraphLeaseV1 {
            locator: VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                canonical_store_locator_digest(&graph_path).expect("graph locator digest"),
            ),
            binding,
            canonical_path: graph_path,
        });
        let registry =
            GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).expect("registry");
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
        let _owner = mount_evaluation_graph_runtime(
            &registry,
            Arc::clone(&operation),
            Arc::clone(&cancellation),
        )
        .expect("owner-mounted evaluation graph");
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = operation;

        registry
            .resolve(GraphDbRegistration {
                authority_lease,
                lifecycle_cancellation: Arc::clone(&cancellation),
                cancellation,
                deadline: Instant::now() + Duration::from_secs(30),
            })
            .expect("registered evaluation operation");
    }

    #[test]
    fn evaluation_graph_refuses_registered_operations_without_an_owner() {
        let root = tempfile::tempdir().expect("evaluation root");
        let graph_path = root
            .path()
            .canonicalize()
            .expect("canonical evaluation root")
            .join("evaluation.grafeo");
        let binding = evaluation_binding().expect("evaluation binding");
        let operation = Arc::new(EvaluationGraphLeaseV1 {
            locator: VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                canonical_store_locator_digest(&graph_path).expect("graph locator digest"),
            ),
            binding,
            canonical_path: graph_path,
        });
        let registry =
            GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).expect("registry");
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = operation;

        let error = registry
            .resolve(GraphDbRegistration {
                authority_lease,
                lifecycle_cancellation: Arc::clone(&cancellation),
                cancellation,
                deadline: Instant::now() + Duration::from_secs(30),
            })
            .expect_err("evaluation operations must not register without an owner");
        assert_eq!(
            error,
            GraphDbError::unavailable("graph runtime is not mounted by its owner attachment"),
            "missing owner is a typed mount refusal, not a transient interrupt"
        );
    }

    #[test]
    fn evaluation_graph_retirement_unmounts_owner_and_releases_capacity() {
        let root = tempfile::tempdir().expect("evaluation root");
        let graph_path = root
            .path()
            .canonicalize()
            .expect("canonical evaluation root")
            .join("evaluation.grafeo");
        let binding = evaluation_binding().expect("evaluation binding");
        let operation = Arc::new(EvaluationGraphLeaseV1 {
            locator: VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                canonical_store_locator_digest(&graph_path).expect("graph locator digest"),
            ),
            binding,
            canonical_path: graph_path,
        });
        let registry =
            GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).expect("registry");
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
        let owner = mount_evaluation_graph_runtime(
            &registry,
            Arc::clone(&operation),
            Arc::clone(&cancellation),
        )
        .expect("owner-mounted evaluation graph");
        assert_eq!(
            registry.capacity().expect("mounted capacity").occupied,
            1,
            "mounted evaluation graph occupies registry capacity"
        );

        let outcome = retire_evaluation_graph_runtime(&registry, &owner)
            .expect("evaluation graph retirement");
        assert!(
            matches!(outcome, GraphDbRetirementOutcome::Closed(_)),
            "successful evaluation retirement must close the exact owner: {outcome:?}"
        );
        drop(owner);

        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = operation;
        let registration = GraphDbRegistration {
            authority_lease,
            lifecycle_cancellation: Arc::clone(&cancellation),
            cancellation,
            deadline: Instant::now() + Duration::from_secs(30),
        };
        assert_eq!(
            registry.status(&registration).expect("retired status"),
            None,
            "successful retirement must remove the registry entry"
        );
        assert_eq!(
            registry.capacity().expect("retired capacity").occupied,
            0,
            "successful retirement must release occupied capacity"
        );
        assert_eq!(
            registry
                .resolve(registration)
                .expect_err("retired evaluation graph must not stay mounted"),
            GraphDbError::unavailable("graph runtime is not mounted by its owner attachment")
        );
    }

    #[test]
    fn retired_evaluation_owner_refuses_new_retains() {
        let error = require_mounted_evaluation_owner(&Mutex::new(None))
            .expect_err("retired owner must not mint a new retain");
        assert_eq!(
            error,
            GraphDbError::unavailable("semantic evaluation graph owner has been retired"),
            "retired Isolated graphs must refuse new retains, not leak a live handle"
        );
    }

    #[test]
    fn failed_retirement_leaks_tempdir_instead_of_unlinking_under_a_live_owner() {
        let root = tempfile::tempdir().expect("evaluation root");
        assert_eq!(
            dispose_evaluation_root(Some(root), false),
            EvaluationRootDisposal::LeakedUntilProcessExit
        );
    }

    #[test]
    fn successful_retirement_releases_the_tempdir() {
        let root = tempfile::tempdir().expect("evaluation root");
        assert_eq!(
            dispose_evaluation_root(Some(root), true),
            EvaluationRootDisposal::Released
        );
    }

    #[test]
    fn closed_evaluation_write_authority_denies_metadata_writes() {
        let authority = EvaluationSqlWriteAuthorityV1 {
            active: AtomicBool::new(true),
        };
        authority
            .verify(ExactSqlWriteIntent::Execute)
            .expect("live Isolated metadata writes must be admitted");
        authority.close();
        let error = authority
            .verify(ExactSqlWriteIntent::Execute)
            .expect_err("retired Isolated graphs must not keep metadata writes live");
        assert!(
            matches!(
                error,
                ExactSqlError::AuthorityDenied(ref message)
                    if message.contains("isolated semantic evaluation authority is closed")
            ),
            "{error:?}"
        );
    }

    #[test]
    fn isolated_pending_cancel_is_blocked_when_publication_already_exists() {
        assert!(
            isolated_pending_stage_cancel_is_blocked(
                &GraphPublicationReplayLookupV1::Missing,
                true
            ),
            "a verified head bound to the stage publication must block Isolated cancel"
        );
        assert!(
            !isolated_pending_stage_cancel_is_blocked(
                &GraphPublicationReplayLookupV1::Missing,
                false
            ),
            "a pending Isolated stage with no replay and no matching head may cancel"
        );
    }

    #[test]
    fn isolated_prepare_skips_native_finalization_when_the_stage_is_cancelled() {
        assert_eq!(
            isolated_native_prepare_gate(true, SemanticVectorStageState::Cancelled),
            IsolatedNativePrepareGate::Cancelled
        );
        assert_eq!(
            isolated_native_prepare_gate(false, SemanticVectorStageState::Pending),
            IsolatedNativePrepareGate::Conflict
        );
        assert_eq!(
            isolated_native_prepare_gate(true, SemanticVectorStageState::Pending),
            IsolatedNativePrepareGate::Incomplete
        );
        assert_eq!(
            isolated_native_prepare_gate(true, SemanticVectorStageState::ReadyToPublish),
            IsolatedNativePrepareGate::ExactReplay
        );
        assert_eq!(
            isolated_native_prepare_gate(true, SemanticVectorStageState::Published),
            IsolatedNativePrepareGate::Conflict
        );
    }

    #[test]
    fn isolated_publish_refuses_cancelled_or_pending_stages() {
        assert_eq!(
            isolated_native_publish_gate(true, SemanticVectorStageState::Cancelled),
            IsolatedNativePublishGate::Conflict
        );
        assert_eq!(
            isolated_native_publish_gate(true, SemanticVectorStageState::Pending),
            IsolatedNativePublishGate::Conflict
        );
        assert_eq!(
            isolated_native_publish_gate(true, SemanticVectorStageState::ReadyToPublish),
            IsolatedNativePublishGate::IsolatedLocal
        );
        assert_eq!(
            isolated_native_publish_gate(true, SemanticVectorStageState::Published),
            IsolatedNativePublishGate::IsolatedLocal
        );
        assert_eq!(
            isolated_native_publish_gate(false, SemanticVectorStageState::Published),
            IsolatedNativePublishGate::Conflict
        );
    }

    #[test]
    fn isolated_recover_and_apply_refuse_a_foreign_projection() {
        assert_eq!(
            isolated_native_recover_publication_gate(false),
            IsolatedNativeRecoverGate::Conflict
        );
        assert_eq!(
            isolated_native_recover_publication_gate(true),
            IsolatedNativeRecoverGate::IsolatedLocal
        );
    }

    #[test]
    fn isolated_settle_refuses_a_cancelled_stage() {
        assert_eq!(
            isolated_native_settle_gate(SemanticVectorStageState::Cancelled),
            IsolatedNativeSettleGate::Conflict
        );
        assert_eq!(
            isolated_native_settle_gate(SemanticVectorStageState::Pending),
            IsolatedNativeSettleGate::Conflict
        );
        assert_eq!(
            isolated_native_settle_gate(SemanticVectorStageState::Published),
            IsolatedNativeSettleGate::IsolatedLocal
        );
    }

    #[test]
    fn mounted_evaluation_owner_allows_retain() {
        let root = tempfile::tempdir().expect("evaluation root");
        let graph_path = root
            .path()
            .canonicalize()
            .expect("canonical evaluation root")
            .join("evaluation.grafeo");
        let binding = evaluation_binding().expect("evaluation binding");
        let operation = Arc::new(EvaluationGraphLeaseV1 {
            locator: VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                canonical_store_locator_digest(&graph_path).expect("graph locator digest"),
            ),
            binding,
            canonical_path: graph_path,
        });
        let registry =
            GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).expect("registry");
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
        let owner = mount_evaluation_graph_runtime(&registry, operation, cancellation)
            .expect("owner-mounted evaluation graph");
        require_mounted_evaluation_owner(&Mutex::new(Some(owner)))
            .expect("mounted owner must still admit Isolated retains");
    }

    #[test]
    fn evaluation_recovery_refuses_missing_replay_before_exact_recovery() {
        let error = require_evaluation_recovery_replay(GraphPublicationReplayLookupV1::Missing)
            .expect_err("missing replay must not enter exact recovery");
        assert_eq!(
            error,
            GraphDbError::Corrupt {
                message: "exact verified graph generation has no durable active replay".to_owned(),
            }
        );
    }

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
