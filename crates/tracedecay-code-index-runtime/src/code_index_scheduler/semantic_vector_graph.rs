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

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{CodeGenerationId, ProjectId};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_store::StoreShardIdV1;

use tracedecay_usecases::store::vector_generations::GraphVectorGenerationStoreV1;

use crate::code_graph_seat::CodeGraphSeatRuntimePortV1;
use tracedecay_usecases::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticRuntimeFuture, SemanticVectorGraphErrorV1,
    SemanticVectorGraphProviderV1, SemanticVectorGraphScopeV1,
    SemanticVectorRetentionAuthorizationV1,
};

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

// `Ready` is read and constructed by field-destructuring match arms across
// several call sites (doctor_kernel, git_watch/store_maintenance,
// retention_inventory); boxing the receipts would ripple through every one
// of them for a cold, infrequently-read diagnostic path.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectVectorReadableSources {
    Ready {
        sources: BTreeSet<CodeGenerationId>,
        configuration_receipt:
            tracedecay_usecases::semantic_runtime::SemanticConfigurationInventoryReceiptV1,
        configured_root_receipt:
            tracedecay_usecases::semantic_runtime::SemanticConfiguredVectorRootReceiptV1,
    },
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

// `Ready` is matched by tuple-destructuring across several call sites
// (doctor_kernel, git_watch/store_maintenance, retention_inventory); boxing
// the census would ripple through every one of them for a cold,
// infrequently-read diagnostic path.
#[allow(clippy::large_enum_variant)]
pub enum ProjectSemanticVectorRetentionStep {
    Ready(tracedecay_graph_db::SemanticVectorRetentionCensus),
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

pub enum ProjectSemanticVectorSourceLiveness {
    Ready(bool),
    ResetRequired(String),
    Corrupt(String),
    Unavailable(String),
    Denied(String),
}

pub enum ProjectSemanticVectorCodeScopeLiveness {
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

/// Converge at most one semantic-vector cleanup or retirement action while
/// returning one bounded project-wide census page.
///
/// The wall span is the latency authority for one retention step; the census
/// gauges and per-outcome counters below record what the step actually
/// decided, so a stalled retirement loop is distinguishable from one that is
/// truthfully finding nothing to retire.
#[hotpath::measure(
    label = "daemon.code_index.semantic_vector.retention.retire",
    future = true
)]
pub async fn retire_one_project_vector_generation(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    configuration: &tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
) -> ProjectSemanticVectorRetentionStep {
    let step =
        converge_one_project_vector_generation(schedulers, project_root, configuration, after)
            .await;
    #[cfg(feature = "hotpath")]
    observe_retention_step(&step);
    step
}

#[cfg(feature = "hotpath")]
fn observe_retention_step(step: &ProjectSemanticVectorRetentionStep) {
    match step {
        ProjectSemanticVectorRetentionStep::Ready(census) => {
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.retire.ready_total")
                .inc(1_u64);
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.census.pending")
                .set(census.pending);
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.census.ready")
                .set(census.ready);
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.census.published")
                .set(census.published);
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.census.cancelled")
                .set(census.cancelled);
        }
        ProjectSemanticVectorRetentionStep::ResetRequired(_) => {
            hotpath::gauge!(
                "daemon.code_index.semantic_vector.retention.retire.reset_required_total"
            )
            .inc(1_u64);
        }
        ProjectSemanticVectorRetentionStep::Corrupt(_) => {
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.retire.corrupt_total")
                .inc(1_u64);
        }
        ProjectSemanticVectorRetentionStep::Unavailable(_) => {
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.retire.unavailable_total")
                .inc(1_u64);
        }
        ProjectSemanticVectorRetentionStep::Denied(_) => {
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.retire.denied_total")
                .inc(1_u64);
        }
    }
}

async fn converge_one_project_vector_generation(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    configuration: &tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
) -> ProjectSemanticVectorRetentionStep {
    let Some(production_runtime) =
        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(project_root)
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
    let store = match GraphVectorGenerationStoreV1::read_only(&retained).await {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).retention_step();
        }
    };
    let step = match store
        .reserve_one_generation(after, Arc::clone(retained.cancellation()))
        .await
    {
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
        } => (census, Some(*reservation)),
    };
    let configuration_receipt = match complete_configuration_inventory(configuration).await {
        Ok(receipt) => receipt,
        Err(failure) => {
            if let Err(error) = release_vector_reservation(&store, reservation.take()).await {
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
            if let Err(error) = release_vector_reservation(&store, reservation.take()).await {
                return ProjectVectorRetentionFailure::from(error).retention_step();
            }
            return failure.retention_step();
        }
    };
    if let Err(error) = store
        .validate_project_census_revision(census.revision, Arc::clone(retained.cancellation()))
        .await
    {
        if let Err(release_error) = release_vector_reservation(&store, reservation.take()).await {
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
            if let Err(error) = store.release_reserved_generation(reservation).await {
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
                    if let Err(error) = release_vector_reservation(&store, Some(reservation)).await
                    {
                        return ProjectVectorRetentionFailure::from(error).retention_step();
                    }
                    return ProjectSemanticVectorRetentionStep::Unavailable(message);
                }
                Err(SemanticVectorGraphErrorV1::Rejected(message)) => {
                    if let Err(error) = release_vector_reservation(&store, Some(reservation)).await
                    {
                        return ProjectVectorRetentionFailure::from(error).retention_step();
                    }
                    return ProjectSemanticVectorRetentionStep::Denied(message);
                }
            };
            match store
                .finalize_reserved_generation(
                    reservation,
                    &authorization,
                    Arc::clone(retained.cancellation()),
                )
                .await
            {
                Ok(action) => {
                    census.action = action;
                    ProjectSemanticVectorRetentionStep::Ready(census)
                }
                Err(error) => ProjectVectorRetentionFailure::from(error).retention_step(),
            }
        }
        Err(error) => {
            if let Err(release_error) = release_vector_reservation(&store, Some(reservation)).await
            {
                return ProjectVectorRetentionFailure::from(release_error).retention_step();
            }
            ProjectVectorRetentionFailure::from_configuration(error).retention_step()
        }
    }
}

async fn release_vector_reservation(
    store: &GraphVectorGenerationStoreV1,
    reservation: Option<tracedecay_graph_db::SemanticVectorRetirementReservation>,
) -> Result<(), tracedecay_usecases::store::vector_generations::VectorGenerationStoreErrorV1> {
    if let Some(reservation) = reservation {
        store.release_reserved_generation(reservation).await?;
    }
    Ok(())
}

/// Resolve one exact indexed source-generation liveness predicate from the
/// project semantic-vector authority. Code-generation deletion must use this
/// under the vector writer freeze; a root-only inventory is insufficient.
pub async fn project_vector_source_generation_is_live(
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
    let store = match GraphVectorGenerationStoreV1::read_only(&retained).await {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).source_liveness();
        }
    };
    match store
        .source_generation_is_live(
            &generation,
            expected_revision,
            Arc::clone(retained.cancellation()),
        )
        .await
    {
        Ok(live) => ProjectSemanticVectorSourceLiveness::Ready(live),
        Err(error) => ProjectVectorRetentionFailure::from(error).source_liveness(),
    }
}

pub async fn project_vector_code_scope_is_live(
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
    let store = match GraphVectorGenerationStoreV1::read_only(&retained).await {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).code_scope_liveness();
        }
    };
    let source_scope = match store
        .source_scope_binding(
            &code_scope_hash,
            expected_revision,
            Arc::clone(retained.cancellation()),
        )
        .await
    {
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
    match store
        .source_scope_is_live(
            &source_scope,
            expected_revision,
            Arc::clone(retained.cancellation()),
        )
        .await
    {
        Ok(live) => ProjectSemanticVectorCodeScopeLiveness::Ready { source_scope, live },
        Err(error) => ProjectVectorRetentionFailure::from(error).code_scope_liveness(),
    }
}

pub async fn remove_project_vector_code_scope_binding(
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
    let store = match GraphVectorGenerationStoreV1::read_only(&retained).await {
        Ok(store) => store,
        Err(error) => {
            return ProjectVectorRetentionFailure::from(error).source_liveness();
        }
    };
    match store
        .remove_source_scope_binding(
            &code_scope_hash,
            source_scope,
            expected_revision,
            Arc::clone(retained.cancellation()),
        )
        .await
    {
        Ok(removed) => ProjectSemanticVectorSourceLiveness::Ready(removed),
        Err(error) => ProjectVectorRetentionFailure::from(error).source_liveness(),
    }
}

/// Observe retention pins through the mounted scheduler's exact registered
/// project/repository/worktree authority.
///
/// This path is deliberately pure: it returns the terminal receipts that
/// justify the inventory and never prunes process-local code handles. Only the
/// daemon maintenance owner may apply that separate mutation after its full
/// revision-bound proof succeeds.
#[hotpath::measure(
    label = "daemon.code_index.semantic_vector.retention.inventory",
    future = true
)]
pub async fn project_vector_readable_sources(
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    configuration: &tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
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
    let store = match GraphVectorGenerationStoreV1::read_only(&retained).await {
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
        Ok((configured_root_receipt, sources)) => {
            hotpath::gauge!("daemon.code_index.semantic_vector.retention.readable_sources")
                .set(sources.len() as u64);
            ProjectVectorReadableSources::Ready {
                sources,
                configuration_receipt,
                configured_root_receipt,
            }
        }
        Err(failure) => failure.readable_sources(),
    }
}

/// Resolve semantic-vector graph runtimes for one mounted project.
pub struct DaemonSemanticVectorGraphProviderV1 {
    project_id: ProjectId,
    project_root: PathBuf,
    schedulers: CodeIndexSchedulerRegistryV1,
    runtime: Arc<dyn CodeGraphSeatRuntimePortV1>,
    project_database: Arc<tracedecay_runtime_core::db::Database>,
}

impl DaemonSemanticVectorGraphProviderV1 {
    pub fn new(
        project_id: ProjectId,
        project_root: PathBuf,
        schedulers: CodeIndexSchedulerRegistryV1,
        runtime: Arc<dyn CodeGraphSeatRuntimePortV1>,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
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

    /// Activation of one semantic-vector graph runtime: replay-binding
    /// resolution plus retaining the code-graph runtime that owns the durable
    /// projection. The graph-side `graph_db.generation.*` spans time the store
    /// work underneath; this span is the daemon activation boundary above it.
    #[hotpath::measure(
        label = "daemon.code_index.semantic_vector.activation.retain",
        future = true
    )]
    /// Retain by generation *identity*, not by a decoded generation: a clean
    /// restart that recovered its retained revision-7 head serves without
    /// seating a second copy of the sealed generation, and the vector graph
    /// binds the same identity either way.
    async fn retain(
        &self,
        scope: &CodeIndexServingScopeV1,
        generation_id: &CodeGenerationId,
        reference: Option<&tracedecay_domain::RefId>,
    ) -> Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1> {
        let replay_binding = self
            .schedulers
            .code_graph_replay_binding(&self.project_root, generation_id)
            .await
            .ok_or_else(|| {
                SemanticVectorGraphErrorV1::Unavailable(
                    "code generation replay authority is not mounted".to_owned(),
                )
            })?
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let lease = self
            .runtime
            .retain_code_graph_runtime(
                self.project_id.clone(),
                scope.repository_id.clone(),
                scope.worktree_id.clone(),
                reference.cloned(),
                generation_id.clone(),
                Arc::clone(&self.project_database),
                replay_binding,
                // This path holds only a borrowed generation; cloning it just to
                // offer it would cost more than the decode it would save.
                None,
            )
            .await
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(SchedulerGraphCancellationV1 {
            shutting_down: Arc::clone(&scope.shutting_down),
        });
        let (project, repository, worktree, source_generation, source_dependency) = lease
            .semantic_vector_identity()
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let semantic_scope = SemanticVectorGraphScopeV1::new(
            project,
            repository,
            worktree,
            source_generation,
            tracedecay_store::SemanticVectorCodeScopeHash::new(
                tracedecay_code_index_retention::code_index_generations::code_index_scope_hash(
                    &self.project_root,
                ),
            )
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?,
            source_dependency,
        )
        .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let _ = lease.semantic_vector_staging_binding();
        let runtime = lease.into_semantic_vector_runtime(semantic_scope);
        Ok(RetainedSemanticVectorGraphV1::new(runtime, cancellation))
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
            self.retain(
                &scope,
                &generation.manifest().generation_id,
                generation.snapshot().reference.as_ref(),
            )
            .await
        })
    }

    fn graph_for_current(
        &self,
    ) -> SemanticRuntimeFuture<'_, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        Box::pin(async move {
            let scope = self.serving_scope().await?;
            if let Some(generation) = scope.serving_generation.clone() {
                return self
                    .retain(
                        &scope,
                        &generation.manifest().generation_id,
                        generation.snapshot().reference.as_ref(),
                    )
                    .await;
            }
            // A clean restart whose retained revision-7 head recovered leaves
            // the sealed seat empty on purpose - replaying the partitions to
            // seat a second copy of what already serves is exactly the cost
            // recovery avoids. Reporting the project as serving nothing here
            // fail-closed every vector retention pass for the life of a quiet
            // checkout, so read the identity from the level that does serve.
            let text = self
                .schedulers
                .latest_text_serving_for_root(&self.project_root)
                .await
                .ok_or_else(|| {
                    SemanticVectorGraphErrorV1::Unavailable(
                        "no code generation is currently serving for this project".to_owned(),
                    )
                })?;
            let metadata = text.metadata();
            self.retain(
                &scope,
                &metadata.manifest().generation_id,
                metadata.snapshot().reference.as_ref(),
            )
            .await
        })
    }
}
