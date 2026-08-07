//! Daemon implementation of the semantic-vector graph provider port.
//!
//! The semantic runtime lives in `tracedecay-usecases` and cannot see daemon
//! session-registry types, so this adapter resolves the mounted worktree's
//! repository/worktree identity and retains the code-graph runtime that owns
//! the durable semantic-vector projection.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{CodeGenerationId, ProjectId};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphWriteBatch, VerifiedGenerationBatchCommit,
    VerifiedGraphSnapshot,
};
use tracedecay_store::{
    GraphPublicationKeyV1, GraphVerifiedHeadV1, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorStageBatchReceipt,
    SemanticVectorStageCancelOutcome, SemanticVectorStageKey, SemanticVectorStagePlan,
    SemanticVectorStagePublicationPrepareOutcome, SemanticVectorStagePublishOutcome,
    SemanticVectorStagePublishSettlement, SemanticVectorStageRecord,
    SemanticVectorStageResumeOutcome, StoreRuntimeBindingV1, StoreShardIdV1,
};

use crate::store::vector_generations::GraphVectorGenerationStoreV1;

use crate::application::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1, SemanticRuntimeFuture,
    SemanticVectorGraphErrorV1, SemanticVectorGraphProviderV1, SemanticVectorGraphScopeV1,
    SemanticVectorRetentionAuthorizationV1, VerifiedSemanticVectorGraphRuntimeV1,
};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;

use super::{CodeIndexSchedulerRegistryV1, registry::CodeIndexServingScopeV1};

mod retention_inventory;
use retention_inventory::{
    ProjectVectorRetentionFailure, complete_configuration_inventory,
    validate_configured_vector_roots,
};

struct SchedulerGraphCancellationV1 {
    shutting_down: Arc<AtomicBool>,
}

impl GraphCancellation for SchedulerGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectVectorReadableSources {
    Ready {
        sources: BTreeSet<CodeGenerationId>,
        configuration_receipt:
            crate::application::semantic_runtime::SemanticConfigurationInventoryReceiptV1,
        configured_root_receipt:
            crate::application::semantic_runtime::SemanticConfiguredVectorRootReceiptV1,
    },
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

pub(crate) enum ProjectSemanticVectorRetentionStep {
    Ready(tracedecay_graph_db::SemanticVectorRetentionCensus),
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

pub(crate) enum ProjectSemanticVectorSourceLiveness {
    Ready(bool),
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

pub(crate) enum ProjectSemanticVectorCodeScopeLiveness {
    Ready {
        source_scope: StoreShardIdV1,
        live: bool,
    },
    Missing,
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

pub(crate) enum ProjectSemanticVectorPublishedDependency {
    Ready(tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup),
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

/// Converge at most one semantic-vector cleanup or retirement action while
/// returning one bounded project-wide census page.
pub(crate) async fn retire_one_project_vector_generation(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    configuration: &crate::application::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
) -> ProjectSemanticVectorRetentionStep {
    let Some(production_runtime) =
        crate::application::semantic_runtime::project_semantic_production_runtime(project_root)
    else {
        return ProjectSemanticVectorRetentionStep::Unavailable(
            "semantic vector mutation authority is not mounted".to_owned(),
        );
    };
    // Freeze publication while roots and the retirement candidate are selected.
    // Activation-vs-retirement correctness additionally comes from the exact
    // VerifiedGraphSnapshot retained through configuration CAS: GraphDb's
    // weak-known lease check returns `Retained` before installing `retiring`.
    let _writer = production_runtime.freeze_vector_mutations().await;
    let Some(provider) = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
    else {
        return ProjectSemanticVectorRetentionStep::Unavailable(
            "semantic vector graph provider is not mounted".to_owned(),
        );
    };
    let retained = match provider.graph_for_current().await {
        Ok(retained) => retained,
        Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
            return ProjectSemanticVectorRetentionStep::Unavailable(message);
        }
        Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
            return ProjectSemanticVectorRetentionStep::Denied(message);
        }
    };
    let store = match GraphVectorGenerationStoreV1::read_only(&retained) {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).retention_step();
        }
    };
    let step = match store.reserve_one_generation(after, Arc::clone(retained.cancellation())) {
        Ok(step) => step,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).retention_step();
        }
    };
    let (mut census, mut reservation) = match step {
        tracedecay_graph_db::SemanticVectorRetentionStep::Census(census) => {
            if !matches!(
                census.action,
                tracedecay_graph_db::SemanticVectorRetentionAction::None
                    | tracedecay_graph_db::SemanticVectorRetentionAction::Retained(_)
            ) {
                return ProjectSemanticVectorRetentionStep::Ready(census);
            }
            (census, None)
        }
        tracedecay_graph_db::SemanticVectorRetentionStep::Reserved {
            census,
            reservation,
        } => (census, Some(reservation)),
    };
    let configuration_receipt = match complete_configuration_inventory(configuration).await {
        Ok(receipt) => receipt,
        Err(failure) => {
            if let Err(error) = release_vector_reservation(&store, reservation.take()) {
                return ProjectVectorRetentionFailure::from(error).retention_step();
            }
            return failure.retention_step();
        }
    };
    let (root_receipt, _) = match validate_configured_vector_roots(
        configuration,
        &store,
        &retained,
        census.revision,
        configuration_receipt.clone(),
    )
    .await
    {
        Ok(receipt) => receipt,
        Err(failure) => {
            if let Err(error) = release_vector_reservation(&store, reservation.take()) {
                return ProjectVectorRetentionFailure::from(error).retention_step();
            }
            return failure.retention_step();
        }
    };
    if let Err(error) =
        store.validate_project_census_revision(census.revision, Arc::clone(retained.cancellation()))
    {
        if let Err(release_error) = release_vector_reservation(&store, reservation.take()) {
            return ProjectVectorRetentionFailure::from(release_error).retention_step();
        }
        return ProjectVectorRetentionFailure::from(error).retention_step();
    }
    let Some(reservation) = reservation else {
        return ProjectSemanticVectorRetentionStep::Ready(census);
    };
    let candidate = reservation.generation_id().clone();
    match configuration
        .is_vector_generation_configured(&root_receipt, &candidate)
        .await
    {
        Ok(true) => {
            if let Err(error) = store.release_reserved_generation(reservation) {
                return ProjectVectorRetentionFailure::from(error).retention_step();
            }
            census.action = tracedecay_graph_db::SemanticVectorRetentionAction::Retained(candidate);
            ProjectSemanticVectorRetentionStep::Ready(census)
        }
        Ok(false) => {
            let authorization = match SemanticVectorRetentionAuthorizationV1::from_terminal_receipts(
                candidate,
                census.revision,
                &configuration_receipt,
                &root_receipt,
            ) {
                Ok(authorization) => authorization,
                Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
                    if let Err(error) = release_vector_reservation(&store, Some(reservation)) {
                        return ProjectVectorRetentionFailure::from(error).retention_step();
                    }
                    return ProjectSemanticVectorRetentionStep::Unavailable(message);
                }
                Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
                    if let Err(error) = release_vector_reservation(&store, Some(reservation)) {
                        return ProjectVectorRetentionFailure::from(error).retention_step();
                    }
                    return ProjectSemanticVectorRetentionStep::Denied(message);
                }
            };
            match store.finalize_reserved_generation(
                reservation,
                &authorization,
                Arc::clone(retained.cancellation()),
            ) {
                Ok(action) => {
                    census.action = action;
                    ProjectSemanticVectorRetentionStep::Ready(census)
                }
                Err(error) => ProjectVectorRetentionFailure::from(error).retention_step(),
            }
        }
        Err(error) => {
            if let Err(release_error) = release_vector_reservation(&store, Some(reservation)) {
                return ProjectVectorRetentionFailure::from(release_error).retention_step();
            }
            ProjectVectorRetentionFailure::from_configuration(error).retention_step()
        }
    }
}

fn release_vector_reservation(
    store: &GraphVectorGenerationStoreV1,
    reservation: Option<tracedecay_graph_db::SemanticVectorRetirementReservation>,
) -> Result<(), crate::store::vector_generations::VectorGenerationStoreErrorV1> {
    if let Some(reservation) = reservation {
        store.release_reserved_generation(reservation)?;
    }
    Ok(())
}

/// Resolve one exact indexed source-generation liveness predicate from the
/// project semantic-vector authority. Code-generation deletion must use this
/// under the vector writer freeze; a root-only inventory is insufficient.
pub(crate) async fn project_vector_source_generation_is_live(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    generation: &CodeGenerationId,
    expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
) -> ProjectSemanticVectorSourceLiveness {
    let Some(provider) = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
    else {
        return ProjectSemanticVectorSourceLiveness::Unavailable(
            "semantic vector graph provider is not mounted".to_owned(),
        );
    };
    let retained = match provider.graph_for_current().await {
        Ok(retained) => retained,
        Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
            return ProjectSemanticVectorSourceLiveness::Unavailable(message);
        }
        Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
            return ProjectSemanticVectorSourceLiveness::Denied(message);
        }
    };
    let generation =
        match tracedecay_store::SemanticVectorSourceGenerationId::new(generation.to_string()) {
            Ok(generation) => generation,
            Err(error) => {
                return ProjectSemanticVectorSourceLiveness::Denied(error.to_string());
            }
        };
    let store = match GraphVectorGenerationStoreV1::read_only(&retained) {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).source_liveness();
        }
    };
    match store.source_generation_is_live(
        &generation,
        expected_revision,
        Arc::clone(retained.cancellation()),
    ) {
        Ok(live) => ProjectSemanticVectorSourceLiveness::Ready(live),
        Err(error) => ProjectVectorRetentionFailure::from(error).source_liveness(),
    }
}

pub(crate) async fn project_vector_source_scope_is_live(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    source_scope: &StoreShardIdV1,
    expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
) -> ProjectSemanticVectorSourceLiveness {
    let Some(provider) = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
    else {
        return ProjectSemanticVectorSourceLiveness::Unavailable(
            "semantic vector graph provider is not mounted".to_owned(),
        );
    };
    let retained = match provider.graph_for_current().await {
        Ok(retained) => retained,
        Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
            return ProjectSemanticVectorSourceLiveness::Unavailable(message);
        }
        Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
            return ProjectSemanticVectorSourceLiveness::Denied(message);
        }
    };
    let store = match GraphVectorGenerationStoreV1::read_only(&retained) {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).source_liveness();
        }
    };
    match store.source_scope_is_live(
        source_scope,
        expected_revision,
        Arc::clone(retained.cancellation()),
    ) {
        Ok(live) => ProjectSemanticVectorSourceLiveness::Ready(live),
        Err(error) => ProjectVectorRetentionFailure::from(error).source_liveness(),
    }
}

pub(crate) async fn project_vector_code_scope_is_live(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    code_scope_hash: &str,
    expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
) -> ProjectSemanticVectorCodeScopeLiveness {
    let code_scope_hash = match tracedecay_store::SemanticVectorCodeScopeHash::new(code_scope_hash)
    {
        Ok(hash) => hash,
        Err(error) => {
            return ProjectSemanticVectorCodeScopeLiveness::Denied(error.to_string());
        }
    };
    let Some(provider) = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
    else {
        return ProjectSemanticVectorCodeScopeLiveness::Unavailable(
            "semantic vector graph provider is not mounted".to_owned(),
        );
    };
    let retained = match provider.graph_for_current().await {
        Ok(retained) => retained,
        Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
            return ProjectSemanticVectorCodeScopeLiveness::Unavailable(message);
        }
        Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
            return ProjectSemanticVectorCodeScopeLiveness::Denied(message);
        }
    };
    let store = match GraphVectorGenerationStoreV1::read_only(&retained) {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).code_scope_liveness();
        }
    };
    let source_scope = match store.source_scope_binding(
        &code_scope_hash,
        expected_revision,
        Arc::clone(retained.cancellation()),
    ) {
        Ok(tracedecay_store::SemanticVectorSourceScopeBindingLookup::Exact(scope)) => scope,
        Ok(tracedecay_store::SemanticVectorSourceScopeBindingLookup::Missing) => {
            return ProjectSemanticVectorCodeScopeLiveness::Missing;
        }
        Ok(tracedecay_store::SemanticVectorSourceScopeBindingLookup::Conflict) => {
            return ProjectSemanticVectorCodeScopeLiveness::Corrupt(
                "code scope has conflicting semantic source bindings".to_owned(),
            );
        }
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).code_scope_liveness();
        }
    };
    match store.source_scope_is_live(
        &source_scope,
        expected_revision,
        Arc::clone(retained.cancellation()),
    ) {
        Ok(live) => ProjectSemanticVectorCodeScopeLiveness::Ready { source_scope, live },
        Err(error) => ProjectVectorRetentionFailure::from(error).code_scope_liveness(),
    }
}

pub(crate) async fn remove_project_vector_code_scope_binding(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    code_scope_hash: &str,
    source_scope: &StoreShardIdV1,
    expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
) -> ProjectSemanticVectorSourceLiveness {
    let code_scope_hash = match tracedecay_store::SemanticVectorCodeScopeHash::new(code_scope_hash)
    {
        Ok(hash) => hash,
        Err(error) => {
            return ProjectSemanticVectorSourceLiveness::Denied(error.to_string());
        }
    };
    let Some(provider) = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
    else {
        return ProjectSemanticVectorSourceLiveness::Unavailable(
            "semantic vector graph provider is not mounted".to_owned(),
        );
    };
    let retained = match provider.graph_for_current().await {
        Ok(retained) => retained,
        Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
            return ProjectSemanticVectorSourceLiveness::Unavailable(message);
        }
        Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
            return ProjectSemanticVectorSourceLiveness::Denied(message);
        }
    };
    let store = match GraphVectorGenerationStoreV1::read_only(&retained) {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).source_liveness();
        }
    };
    match store.remove_source_scope_binding(
        &code_scope_hash,
        source_scope,
        expected_revision,
        Arc::clone(retained.cancellation()),
    ) {
        Ok(removed) => ProjectSemanticVectorSourceLiveness::Ready(removed),
        Err(error) => ProjectVectorRetentionFailure::from(error).source_liveness(),
    }
}

pub(crate) async fn project_vector_published_dependency(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    generation: &tracedecay_domain::VectorGenerationIdV1,
    expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
) -> ProjectSemanticVectorPublishedDependency {
    let Some(provider) = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
    else {
        return ProjectSemanticVectorPublishedDependency::Unavailable(
            "semantic vector graph provider is not mounted".to_owned(),
        );
    };
    let retained = match provider.graph_for_current().await {
        Ok(retained) => retained,
        Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
            return ProjectSemanticVectorPublishedDependency::Unavailable(message);
        }
        Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
            return ProjectSemanticVectorPublishedDependency::Denied(message);
        }
    };
    let store = match GraphVectorGenerationStoreV1::read_only(&retained) {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).published_dependency();
        }
    };
    match store.published_generation_dependency(
        generation,
        expected_revision,
        Arc::clone(retained.cancellation()),
    ) {
        Ok(dependency) => ProjectSemanticVectorPublishedDependency::Ready(dependency),
        Err(error) => ProjectVectorRetentionFailure::from(error).published_dependency(),
    }
}

/// Observe retention pins through the mounted scheduler's exact registered
/// project/repository/worktree authority.
///
/// This path is deliberately pure: it returns the terminal receipts that
/// justify the inventory and never prunes process-local code handles. Only the
/// daemon maintenance owner may apply that separate mutation after its full
/// revision-bound proof succeeds.
pub(crate) async fn project_vector_readable_sources(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    configuration: &crate::application::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
) -> ProjectVectorReadableSources {
    let Some(provider) = schedulers
        .semantic_vector_graph_provider(project_root)
        .await
    else {
        return ProjectVectorReadableSources::Unavailable(
            "semantic vector graph provider is not mounted".to_owned(),
        );
    };
    let retained = match provider.graph_for_current().await {
        Ok(retained) => retained,
        Err(SemanticVectorGraphErrorV1::Unavailable(message)) => {
            return ProjectVectorReadableSources::Unavailable(message);
        }
        Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
            return ProjectVectorReadableSources::Denied(message);
        }
    };
    let store = match GraphVectorGenerationStoreV1::read_only(&retained) {
        Ok(store) => store,
        Err(error) => return ProjectVectorRetentionFailure::from(error).readable_sources(),
    };
    let configuration_receipt = match complete_configuration_inventory(configuration).await {
        Ok(inventory) => inventory,
        Err(failure) => return failure.readable_sources(),
    };
    match validate_configured_vector_roots(
        configuration,
        &store,
        &retained,
        expected_revision,
        configuration_receipt.clone(),
    )
    .await
    {
        Ok((configured_root_receipt, sources)) => ProjectVectorReadableSources::Ready {
            sources,
            configuration_receipt,
            configured_root_receipt,
        },
        Err(failure) => failure.readable_sources(),
    }
}

/// Resolve semantic-vector graph runtimes for one mounted project.
pub(crate) struct DaemonSemanticVectorGraphProviderV1 {
    project_id: ProjectId,
    project_root: PathBuf,
    schedulers: CodeIndexSchedulerRegistryV1,
    runtime: Arc<DaemonSessionRuntimeRegistryV1>,
    project_database: Arc<crate::db::Database>,
}

impl DaemonSemanticVectorGraphProviderV1 {
    pub(crate) fn new(
        project_id: ProjectId,
        project_root: PathBuf,
        schedulers: CodeIndexSchedulerRegistryV1,
        runtime: Arc<DaemonSessionRuntimeRegistryV1>,
        project_database: Arc<crate::db::Database>,
    ) -> Self {
        Self {
            project_id,
            project_root,
            schedulers,
            runtime,
            project_database,
        }
    }

    async fn serving_scope(&self) -> Result<CodeIndexServingScopeV1, SemanticVectorGraphErrorV1> {
        self.schedulers
            .serving_code_scope(&self.project_root)
            .await
            .ok_or_else(|| {
                SemanticVectorGraphErrorV1::Unavailable(
                    "code-index worktree is not mounted for this project".to_owned(),
                )
            })
    }

    async fn retain(
        &self,
        scope: &CodeIndexServingScopeV1,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1> {
        let replay_binding = self
            .schedulers
            .code_graph_replay_binding(&self.project_root, &generation.manifest().generation_id)
            .await
            .ok_or_else(|| {
                SemanticVectorGraphErrorV1::Unavailable(
                    "code generation replay authority is not mounted".to_owned(),
                )
            })?
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let retained = self
            .runtime
            .retain_code_graph_runtime(
                self.project_id.clone(),
                scope.repository_id.clone(),
                scope.worktree_id.clone(),
                generation.snapshot().reference.clone(),
                generation.manifest().generation_id.clone(),
                Arc::clone(&self.project_database),
                replay_binding,
            )
            .await
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(SchedulerGraphCancellationV1 {
            shutting_down: Arc::clone(&scope.shutting_down),
        });
        let (project, repository, worktree, source_generation, source_dependency) = retained
            .semantic_vector_identity()
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let semantic_scope = SemanticVectorGraphScopeV1::new(
            project,
            repository,
            worktree,
            source_generation,
            tracedecay_store::SemanticVectorCodeScopeHash::new(
                crate::retention::code_index_generations::code_index_scope_hash(&self.project_root),
            )
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?,
            source_dependency,
        )
        .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let (source_scope, binding) = retained.semantic_vector_staging_binding();
        let runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1> =
            Arc::new(DaemonVerifiedSemanticVectorGraphRuntimeV1 {
                source_scope: source_scope.clone(),
                binding: binding.clone(),
                retained: Arc::new(retained),
                scope: semantic_scope,
            });
        Ok(RetainedSemanticVectorGraphV1::new(runtime, cancellation))
    }
}

struct DaemonVerifiedSemanticVectorGraphRuntimeV1 {
    retained: Arc<crate::daemon::store_runtime::session_registry::RetainedCodeGraphRuntimeV1>,
    scope: SemanticVectorGraphScopeV1,
    source_scope: StoreShardIdV1,
    binding: StoreRuntimeBindingV1,
}

impl VerifiedSemanticVectorGraphRuntimeV1 for DaemonVerifiedSemanticVectorGraphRuntimeV1 {
    fn scope(&self) -> &SemanticVectorGraphScopeV1 {
        &self.scope
    }

    fn recover_verified_snapshot(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        self.retained.recover_semantic_vector_projection(
            self.scope.projection(),
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn staging_binding(&self) -> (&StoreShardIdV1, &StoreRuntimeBindingV1) {
        (&self.source_scope, &self.binding)
    }

    fn recover_verified_generation(
        &self,
        publication: &GraphPublicationKeyV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.retained.recover_semantic_vector_generation(
            publication,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn verified_head(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<GraphVerifiedHeadV1>, GraphDbError> {
        self.retained.semantic_vector_verified_head(
            self.scope.projection(),
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn begin_stage(
        &self,
        plan: &SemanticVectorStagePlan,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageRecord, GraphDbError> {
        self.retained.begin_semantic_vector_stage(
            plan,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn resume_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageResumeOutcome, GraphDbError> {
        self.retained.resume_semantic_vector_stage(
            stage,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn published_semantic_generation(
        &self,
        key: &SemanticVectorPublishedGenerationKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorPublishedGenerationLookup, GraphDbError> {
        self.retained.published_semantic_vector_generation(
            key,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn append_stage_batch(
        &self,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBatchCommit, GraphDbError> {
        self.retained.append_semantic_vector_stage_batch(
            receipt,
            batch,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn cancel_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageCancelOutcome, GraphDbError> {
        self.retained.cancel_semantic_vector_stage(
            stage,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn prepare_publication_from_staged_native(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublicationPrepareOutcome, GraphDbError> {
        self.retained
            .prepare_semantic_vector_publication_from_staged_native(
                stage,
                authority.cancellation(),
                authority.deadline(),
            )
    }

    fn publish_ready_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.retained.publish_ready_semantic_vector_stage(
            stage,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn settle_published(
        &self,
        settlement: &SemanticVectorStagePublishSettlement,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublishOutcome, GraphDbError> {
        self.retained.settle_published_semantic_vector_stage(
            settlement,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn reserve_one_generation(
        &self,
        after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionStep, GraphDbError> {
        self.retained.reserve_one_semantic_vector_generation(
            after,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn finalize_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
        authorization: &SemanticVectorRetentionAuthorizationV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionAction, GraphDbError> {
        self.retained.finalize_reserved_semantic_vector_generation(
            reservation,
            authorization,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn release_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
    ) -> Result<(), GraphDbError> {
        self.retained
            .release_reserved_semantic_vector_generation(reservation)
    }

    fn source_generation_has_live_reference(
        &self,
        generation: &tracedecay_store::SemanticVectorSourceGenerationId,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.retained.semantic_vector_source_generation_is_live(
            generation,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn source_scope_has_live_reference(
        &self,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.retained.semantic_vector_source_scope_is_live(
            source_scope,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn published_generation_dependency(
        &self,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup, GraphDbError>
    {
        self.retained
            .semantic_vector_published_generation_dependency(
                generation,
                expected_revision,
                authority.cancellation(),
                authority.deadline(),
            )
    }

    fn validate_project_census_revision(
        &self,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<(), GraphDbError> {
        self.retained
            .validate_semantic_vector_project_census_revision(
                expected_revision,
                authority.cancellation(),
                authority.deadline(),
            )
    }

    fn source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorSourceScopeBindingLookup, GraphDbError> {
        self.retained.semantic_vector_source_scope_binding(
            code_scope_hash,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn remove_source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.retained.remove_semantic_vector_source_scope_binding(
            code_scope_hash,
            source_scope,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }
}

impl SemanticVectorGraphProviderV1 for DaemonSemanticVectorGraphProviderV1 {
    fn graph_for_generation<'a>(
        &'a self,
        generation: &'a CodeIndexPublishedGenerationV1,
    ) -> SemanticRuntimeFuture<'a, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        Box::pin(async move {
            let scope = self.serving_scope().await?;
            self.retain(&scope, generation).await
        })
    }

    fn graph_for_current(
        &self,
    ) -> SemanticRuntimeFuture<'_, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        Box::pin(async move {
            let scope = self.serving_scope().await?;
            let generation = scope.serving_generation.clone().ok_or_else(|| {
                SemanticVectorGraphErrorV1::Unavailable(
                    "no code generation is currently serving for this project".to_owned(),
                )
            })?;
            self.retain(&scope, generation.as_ref()).await
        })
    }
}
