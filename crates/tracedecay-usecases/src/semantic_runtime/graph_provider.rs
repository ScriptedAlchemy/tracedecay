//! Typed port giving semantic vector storage the daemon's canonical verified
//! graph registry and relational replay/CAS authority.

use std::sync::Arc;
use std::time::Instant;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, VectorGenerationIdV1, WorktreeId,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphGenerationDependency, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphWriteBatch, VerifiedGenerationBatchCommit, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    GraphPublicationKeyV1, GraphVerifiedHeadV1, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorStageBatchReceipt,
    SemanticVectorStageCancelOutcome, SemanticVectorStageCensusRevision, SemanticVectorStageKey,
    SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageResumeOutcome, StoreRuntimeBindingV1,
    StoreShardIdV1,
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
    ) -> Result<SemanticVectorStageRecord, GraphDbError>;

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

/// A code-graph authority retained for semantic-vector reads and writes.
pub struct RetainedSemanticVectorGraphV1 {
    runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
    cancellation: Arc<dyn GraphCancellation>,
}

impl RetainedSemanticVectorGraphV1 {
    pub fn new(
        runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Self {
        Self {
            runtime,
            cancellation,
        }
    }

    pub fn runtime(&self) -> &Arc<dyn VerifiedSemanticVectorGraphRuntimeV1> {
        &self.runtime
    }

    pub fn cancellation(&self) -> &Arc<dyn GraphCancellation> {
        &self.cancellation
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
