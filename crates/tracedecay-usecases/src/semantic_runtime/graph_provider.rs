//! Typed port giving semantic vector storage the daemon's canonical verified
//! graph registry and relational replay/CAS authority.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use tokio::sync::watch;
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, VectorGenerationIdV1, WorktreeId,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphGenerationDependency, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphWriteBatch, VerifiedGenerationBatchCommit,
    VerifiedGenerationBeginV1, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    GraphPublicationKeyV1, GraphVerifiedHeadV1, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorStageBatchReceipt,
    SemanticVectorStageCancelOutcome, SemanticVectorStageCensusRevision, SemanticVectorStageKey,
    SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageResumeOutcome, StoreRuntimeBindingV1, StoreShardIdV1,
};

use super::config_inventory::{
    SemanticConfigurationInventoryReceiptV1, SemanticConfiguredVectorRootReceiptV1,
};
use super::ports::SemanticRuntimeFuture;

#[derive(Clone)]
pub struct SemanticGraphExecutionAuthorityV1 {
    cancellation: Arc<dyn GraphCancellation>,
    deadline: Instant,
}

impl SemanticGraphExecutionAuthorityV1 {
    pub fn new(cancellation: Arc<dyn GraphCancellation>, deadline: Instant) -> Self {
        Self {
            cancellation,
            deadline,
        }
    }

    pub fn checkpoint(&self) -> Result<(), GraphDbError> {
        if self.cancellation.is_cancelled() {
            Err(GraphDbError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(GraphDbError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }

    pub fn cancellation(&self) -> Arc<dyn GraphCancellation> {
        Arc::clone(&self.cancellation)
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

/// Why a code-graph runtime could not be retained for semantic-vector use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorGraphErrorV1 {
    /// No mounted code-graph runtime currently serves the project scope.
    Unavailable(String),
    /// The graph authority rejected the retention request.
    Rejected(String),
}

impl std::fmt::Display for SemanticVectorGraphErrorV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => {
                write!(formatter, "semantic vector graph unavailable: {reason}")
            }
            Self::Rejected(reason) => {
                write!(formatter, "semantic vector graph rejected: {reason}")
            }
        }
    }
}

impl std::error::Error for SemanticVectorGraphErrorV1 {}

/// Complete configuration liveness authorization for one reserved semantic
/// vector generation.
///
/// Callers cannot construct this from a root set or raw digest. Both receipts
/// are terminal capabilities minted by the production configuration authority,
/// and the graph runtime separately binds the reservation to the same candidate
/// and stage-census revision before any retirement mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorRetentionAuthorizationV1 {
    candidate: VectorGenerationIdV1,
    stage_revision: SemanticVectorStageCensusRevision,
    configuration_revision: u64,
    configuration_inventory_digest: ManifestDigest,
    configured_root_digest: ManifestDigest,
    configured_root_count: u64,
}

impl SemanticVectorRetentionAuthorizationV1 {
    pub fn from_terminal_receipts(
        candidate: VectorGenerationIdV1,
        stage_revision: SemanticVectorStageCensusRevision,
        configuration: &SemanticConfigurationInventoryReceiptV1,
        configured_roots: &SemanticConfiguredVectorRootReceiptV1,
    ) -> Result<Self, SemanticVectorGraphErrorV1> {
        candidate.validate().map_err(|error| {
            SemanticVectorGraphErrorV1::Rejected(format!(
                "semantic vector retirement candidate is invalid: {error}"
            ))
        })?;
        if configuration.revision() != configured_roots.revision() {
            return Err(SemanticVectorGraphErrorV1::Rejected(
                "semantic vector retention configuration receipts disagree on revision".to_owned(),
            ));
        }
        if configured_roots.root_count() > configuration.root_binding_count() {
            return Err(SemanticVectorGraphErrorV1::Rejected(
                "semantic vector retention root receipt exceeds configuration coverage".to_owned(),
            ));
        }
        Ok(Self {
            candidate,
            stage_revision,
            configuration_revision: configuration.revision(),
            configuration_inventory_digest: configuration.inventory_digest().clone(),
            configured_root_digest: configured_roots.root_digest().clone(),
            configured_root_count: configured_roots.root_count(),
        })
    }

    pub fn candidate(&self) -> &VectorGenerationIdV1 {
        &self.candidate
    }

    pub fn stage_revision(&self) -> SemanticVectorStageCensusRevision {
        self.stage_revision
    }

    pub fn configuration_revision(&self) -> u64 {
        self.configuration_revision
    }

    pub fn configuration_inventory_digest(&self) -> &ManifestDigest {
        &self.configuration_inventory_digest
    }

    pub fn configured_root_digest(&self) -> &ManifestDigest {
        &self.configured_root_digest
    }

    pub fn configured_root_count(&self) -> u64 {
        self.configured_root_count
    }
}

/// Exact mutable-checkout and source-generation identity for one vector
/// projection. Including the worktree prevents linked worktrees in a shared
/// project store from replacing one another's active generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticVectorGraphScopeV1 {
    pub project: ProjectId,
    pub repository: RepositoryId,
    pub worktree: WorktreeId,
    pub source_generation: CodeGenerationId,
    code_scope_hash: tracedecay_store::SemanticVectorCodeScopeHash,
    projection: GraphProjectionIdentity,
    source_dependency: GraphGenerationDependency,
}

impl SemanticVectorGraphScopeV1 {
    pub fn new(
        project: ProjectId,
        repository: RepositoryId,
        worktree: WorktreeId,
        source_generation: CodeGenerationId,
        code_scope_hash: tracedecay_store::SemanticVectorCodeScopeHash,
        source_dependency: GraphGenerationDependency,
    ) -> Result<Self, GraphDbError> {
        let digest = canonical_sha256(&(
            "tracedecay.semantic-vector.graph-scope.v2",
            &project,
            &repository,
            &worktree,
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new(format!("semantic-vector:{}", digest.as_str()))?,
            GraphProjectionId::new(
                crate::store::vector_generations::SEMANTIC_VECTOR_GRAPH_PROJECTION,
            )?,
        );
        Ok(Self {
            project,
            repository,
            worktree,
            source_generation,
            code_scope_hash,
            projection,
            source_dependency,
        })
    }

    pub fn projection(&self) -> &GraphProjectionIdentity {
        &self.projection
    }

    pub fn source_dependency(&self) -> &GraphGenerationDependency {
        &self.source_dependency
    }

    pub fn code_scope_hash(&self) -> &tracedecay_store::SemanticVectorCodeScopeHash {
        &self.code_scope_hash
    }
}

/// Daemon-owned verified publication and restart-recovery authority.
///
/// Implementations append the complete manifest to the canonical relational
/// replay/outbox before invoking registry verification and head CAS. This port
/// deliberately exposes no raw graph database handle.
pub trait VerifiedSemanticVectorGraphRuntimeV1: Send + Sync {
    fn scope(&self) -> &SemanticVectorGraphScopeV1;

    fn recover_verified_snapshot(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError>;

    fn recover_verified_generation(
        &self,
        publication: &GraphPublicationKeyV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError>;

    fn staging_binding(&self) -> (&StoreShardIdV1, &StoreRuntimeBindingV1);

    fn verified_head(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<GraphVerifiedHeadV1>, GraphDbError>;

    fn begin_stage(
        &self,
        plan: &SemanticVectorStagePlan,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBeginV1, GraphDbError>;

    fn resume_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageResumeOutcome, GraphDbError>;

    fn published_semantic_generation(
        &self,
        key: &SemanticVectorPublishedGenerationKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorPublishedGenerationLookup, GraphDbError>;

    fn append_stage_batch(
        &self,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBatchCommit, GraphDbError>;

    fn cancel_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageCancelOutcome, GraphDbError>;

    fn prepare_publication_from_staged_native(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublicationPrepareOutcome, GraphDbError>;

    fn publish_ready_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError>;

    fn settle_published(
        &self,
        settlement: &SemanticVectorStagePublishSettlement,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublishOutcome, GraphDbError>;

    fn reserve_one_generation(
        &self,
        after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionStep, GraphDbError>;

    fn finalize_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
        authorization: &SemanticVectorRetentionAuthorizationV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionAction, GraphDbError>;

    fn release_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
    ) -> Result<(), GraphDbError>;

    fn source_generation_has_live_reference(
        &self,
        generation: &tracedecay_store::SemanticVectorSourceGenerationId,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError>;

    fn source_scope_has_live_reference(
        &self,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError>;

    fn published_generation_dependency(
        &self,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup, GraphDbError>;

    fn validate_project_census_revision(
        &self,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<(), GraphDbError>;

    fn source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorSourceScopeBindingLookup, GraphDbError>;

    fn remove_source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError>;
}

struct SemanticVectorOperationTaskStateV1 {
    accepting: bool,
    next_task_id: u64,
    tasks: BTreeMap<u64, tokio::task::JoinHandle<()>>,
    shutdown_completion: Option<watch::Receiver<Option<Result<(), String>>>>,
}

struct SemanticVectorOperationTaskFinalizerV1 {
    state: Weak<Mutex<SemanticVectorOperationTaskStateV1>>,
    task_id: u64,
    completed: bool,
}

impl Drop for SemanticVectorOperationTaskFinalizerV1 {
    fn drop(&mut self) {
        if !self.completed {
            return;
        }
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tasks
            .remove(&self.task_id);
    }
}

/// Lifecycle owner for semantic-vector operation settlement tasks.
///
/// Admission and task publication share one synchronous mutex boundary, so
/// `begin_shutdown` fences every later operation before shutdown atomically
/// takes and joins all previously retained settlement tasks.
pub struct SemanticVectorOperationTaskOwnerV1 {
    state: Arc<Mutex<SemanticVectorOperationTaskStateV1>>,
}

impl SemanticVectorOperationTaskOwnerV1 {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SemanticVectorOperationTaskStateV1 {
                accepting: true,
                next_task_id: 0,
                tasks: BTreeMap::new(),
                shutdown_completion: None,
            })),
        }
    }

    /// Synchronously admits and spawns one settlement future.
    ///
    /// `false` means admission was fenced, no Tokio runtime was available, or
    /// the monotonic task identity space was exhausted. In every case the
    /// supplied future is dropped without being polled.
    pub fn retain<Fut>(&self, settlement: Fut) -> bool
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return false;
        }
        let Some(task_id) = state.next_task_id.checked_add(1) else {
            return false;
        };
        state.next_task_id = task_id;
        let finalizer_state = Arc::downgrade(&self.state);
        let task = runtime.spawn(async move {
            let mut finalizer = SemanticVectorOperationTaskFinalizerV1 {
                state: finalizer_state,
                task_id,
                completed: false,
            };
            settlement.await;
            finalizer.completed = true;
        });
        state.tasks.insert(task_id, task);
        true
    }

    pub fn begin_shutdown(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accepting = false;
    }

    /// Fences admission and joins every settlement task retained before the
    /// fence. Operation results are delivered separately to live callers, so
    /// only an actual settlement-task join failure is reported here.
    pub async fn shutdown(&self) -> Result<(), String> {
        let mut completion = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.accepting = false;
            if let Some(completion) = state.shutdown_completion.clone() {
                completion
            } else {
                let tasks = std::mem::take(&mut state.tasks);
                let (publish_completion, completion) = watch::channel(None);
                state.shutdown_completion = Some(completion.clone());
                drop(tokio::spawn(async move {
                    let mut failures = Vec::new();
                    for (task_id, task) in tasks {
                        if let Err(error) = task.await {
                            failures.push(format!(
                                "semantic vector operation settlement task {task_id} join failed: {error}"
                            ));
                        }
                    }
                    let result = if failures.is_empty() {
                        Ok(())
                    } else {
                        Err(failures.join("; "))
                    };
                    publish_completion.send_replace(Some(result));
                }));
                completion
            }
        };

        loop {
            if let Some(result) = completion.borrow().clone() {
                return result;
            }
            if completion.changed().await.is_err() {
                if let Some(result) = completion.borrow().clone() {
                    return result;
                }
                return Err(
                    "semantic vector operation settlement reaper ended without publishing shutdown completion"
                        .to_owned(),
                );
            }
        }
    }
}

impl Default for SemanticVectorOperationTaskOwnerV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SemanticVectorOperationTaskOwnerV1 {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        // A started shutdown has already transferred its handles to the
        // detached reaper. Only tasks still owned by this map may be aborted.
        for (_, task) in std::mem::take(&mut state.tasks) {
            task.abort();
        }
    }
}

/// A code-graph authority retained for semantic-vector reads and writes.
pub struct RetainedSemanticVectorGraphV1 {
    runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
    cancellation: Arc<dyn GraphCancellation>,
    operation_task_owner: Arc<SemanticVectorOperationTaskOwnerV1>,
}

impl RetainedSemanticVectorGraphV1 {
    pub fn new(
        runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Self {
        Self::new_with_operation_task_owner(
            runtime,
            cancellation,
            Arc::new(SemanticVectorOperationTaskOwnerV1::new()),
        )
    }

    pub fn new_with_operation_task_owner(
        runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
        cancellation: Arc<dyn GraphCancellation>,
        operation_task_owner: Arc<SemanticVectorOperationTaskOwnerV1>,
    ) -> Self {
        Self {
            runtime,
            cancellation,
            operation_task_owner,
        }
    }

    pub fn runtime(&self) -> &Arc<dyn VerifiedSemanticVectorGraphRuntimeV1> {
        &self.runtime
    }

    pub fn cancellation(&self) -> &Arc<dyn GraphCancellation> {
        &self.cancellation
    }

    pub(crate) fn operation_task_owner(&self) -> &Arc<SemanticVectorOperationTaskOwnerV1> {
        &self.operation_task_owner
    }
}

/// Daemon-implemented resolution from code-generation identity to the retained
/// graph authority that stores that exact checkout's semantic vectors.
pub trait SemanticVectorGraphProviderV1: Send + Sync {
    fn graph_for_generation<'a>(
        &'a self,
        generation: &'a CodeIndexPublishedGenerationV1,
    ) -> SemanticRuntimeFuture<'a, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>;

    fn graph_for_current(
        &self,
    ) -> SemanticRuntimeFuture<'_, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::sync::Notify;

    #[tokio::test]
    async fn operation_owner_shutdown_fences_admission_and_joins_retained_work() {
        let owner = Arc::new(SemanticVectorOperationTaskOwnerV1::new());
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        assert!(owner.retain({
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            async move {
                started.notify_one();
                release.notified().await;
            }
        }));
        started.notified().await;

        owner.begin_shutdown();
        assert!(
            !owner.retain(async {}),
            "shutdown must fence later settlement admission"
        );
        let first_shutdown = tokio::spawn({
            let owner = Arc::clone(&owner);
            async move { owner.shutdown().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !first_shutdown.is_finished(),
            "first shutdown waiter must join retained settlement work"
        );

        first_shutdown.abort();
        let first_join_error = match first_shutdown.await {
            Err(error) => error,
            Ok(_) => panic!("first shutdown waiter unexpectedly completed"),
        };
        assert!(
            first_join_error.is_cancelled(),
            "aborted first shutdown waiter must join as cancelled"
        );

        let retry_shutdown = tokio::spawn({
            let owner = Arc::clone(&owner);
            async move { owner.shutdown().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !retry_shutdown.is_finished(),
            "retried shutdown must retain and join settlement work after waiter cancellation"
        );

        release.notify_one();
        retry_shutdown
            .await
            .expect("retried operation-owner shutdown remains joinable")
            .expect("retried operation-owner shutdown joins cleanly");
    }
}
