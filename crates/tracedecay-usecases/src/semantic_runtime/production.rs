//! Production bridge between daemon semantic scheduling and application search.
//!
//! Saved code generations call [`schedule_saved_code_generation`] without waiting
//! for `FastEmbed` download/indexing. Application search admits a semantic lane
//! only through [`query_factory`] once a complete compatible generation is
//! atomically current. Status projection carries indexing progress, degraded
//! reason, and prior generation for Doctor/`tracedecay_runtime`.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_application::now_micros;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkV1,
    CompactCandidate, ComponentRevision, EvidenceRole, FixedPointScore, LogicalEvidenceId,
    ManifestDigest, ProjectionBatchRequestV1, ProjectionOperationV1, ProjectionReplayReasonV1,
    QueryFallbackSubpayload, RetrievalAnchorId, RetrievalCursorKeyId, RetrieverBatch,
    RetrieverKind, RetrieverOutcome, ScoreDomainId, SemanticSearchIndexKeyV1,
    SemanticSearchIndexProfileV1, SourceOccurrenceId, VectorGenerationIdV1, WorktreeId,
    canonical_sha256,
};
use tracedecay_policy::retrieval_selection::{
    RetrievalAvailabilityV1, RetrievalRequirementV1, RetrievalSelectionV1, select_retrieval,
};

use crate::config::SemanticResourceCeilings;
use crate::retention::code_index_generations::{
    CodeGenerationRetentionModeV1, CodeGenerationRetentionReceiptV1,
    DEFAULT_SUPERSEDED_GENERATION_FLOOR, execute_code_generation_retention,
    plan_code_generation_retention, recover_code_generation_retention,
};
use crate::store::vector_generations::{
    DatabaseLegacyVectorInventoryV1, DatabaseVectorEvaluationStoreV1,
    DatabaseVectorGenerationStoreV1, FakeVectorGenerationStoreV1, PublishedVectorGenerationV1,
    VectorGenerationPlanV1,
};
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_code_index::projection::expected_request_digest;
use tracedecay_query::retrieval::AuthorizedQueryFallbackV1;
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
use tracedecay_query::retrieval::graph::production_code_index_freshness;
use tracedecay_query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, RetrievalPortError,
};
use tracedecay_query::retrieval::rerank::RerankExecutionControlV1;
use tracedecay_query::retrieval::semantic::{
    CalibratedSemanticQueryService, CodeSemanticEvidenceV1, CompleteSemanticGenerationV1,
    SemanticAbstentionDispositionV1, SemanticCalibrationProfileV1, SemanticCodeRetriever,
    SemanticExecutionControl, SemanticIndexStateV1, SemanticLaneReadinessV1, SemanticLaneRetriever,
    SemanticQueryDecisionV1, SemanticQueryModeV1, SemanticQueryServiceError,
    SemanticQueryServiceOutcomeV1, SemanticRetrievalRequestV1, SemanticSearchKindV1,
    SemanticVectorReadPort, SemanticVectorReadRequestV1, SemanticVectorRecordV1,
    SemanticVectorScanSummaryV1,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_search_eval::candidate_output::ProductionCandidateSemanticProjectionSourcesV1;
use tracedecay_search_eval::semantic_native::{
    SemanticProjectionCaseOutcomeV1, SemanticProjectionCaseSampleV1, SemanticProjectionCaseV1,
};
use tracedecay_search_eval::{
    CandidateOutputError, ProductionCandidateNativeGenerationResourcesV1,
    ProductionCandidateNativeQueryContextV1, ProductionCandidateNativeQueryInputsV1,
};
use tracedecay_semantic::legacy_migration::{
    CanonicalEligibleChunkSetV1, LegacyVectorInventoryPortV1, LegacyVectorMigrationErrorV1,
    LegacyVectorMigrationOwnerTransactionV1, LegacyVectorMigrationReceiptV1,
    NeverCancelLegacyVectorMigrationV1, ProductionLegacyVectorCanonicalRebuilderV1,
    StagedCanonicalVectorRebuildV1, prepare_legacy_vector_migration,
};
use tracedecay_semantic::projector::PreparedVectorGenerationV1;
use tracedecay_semantic::rerank_adapter::{
    GenerationBoundCodeRerankViewsV1, ProductionCodeRerankAuthorityV1,
};
use tracedecay_semantic::{
    DaemonSemanticQueryFactoryV1, DaemonSemanticRuntimeHandleV1,
    FastEmbedSemanticGenerationRequestV1, LoadedSemanticArtifactV1,
    PreparedSemanticEvaluationProjectionV1, PreparedSemanticRuntimeCommitV1,
    SemanticGenerationPointerV1, SemanticModelLifecycleOwnerV1, SemanticRuntimeScheduleFailureV1,
    SemanticRuntimeScheduleStatusV1, SemanticRuntimeStatusProjectionV1,
    prepare_semantic_evaluation_projection,
};

#[cfg(test)]
use super::ports::SemanticActivationRequestV1;
use super::ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticConfigurationPinV1,
    SemanticExecutableGenerationV1, SemanticFallbackReasonV1, SemanticRollbackCommandV1,
    SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1,
    SemanticRuntimeFuture, SemanticRuntimeGenerationInspectorV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};
use super::{
    DaemonGlobalSemanticProjectionSchedulerV1, SemanticProjectionBatchV1,
    SemanticProjectionLeaseV1, SemanticProjectionScheduleErrorV1,
};

/// Map daemon schedule projection into the application/Doctor status shape.
///
/// Indexing never blocks exact/lexical/graph; the route remains lexical until
/// [`SemanticRuntimeStateV1::Current`].
pub fn application_status_from_projection(
    projection: &SemanticRuntimeStatusProjectionV1,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    let state = match &projection.status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticRuntimeStateV1::Unavailable {
            reason: projection
                .degraded_reason
                .unwrap_or(SemanticFallbackReasonV1::RuntimeUnavailable),
        },
        SemanticRuntimeScheduleStatusV1::Indexing {
            target_generation,
            completed_units,
            total_units,
            ..
        } => SemanticRuntimeStateV1::Indexing {
            target_generation: provisional_vector_generation(target_generation),
            completed_units: *completed_units,
            total_units: *total_units,
        },
        SemanticRuntimeScheduleStatusV1::Failed {
            reason,
            prior_generation,
        } => SemanticRuntimeStateV1::Degraded {
            active_generation: prior_generation
                .clone()
                .or_else(|| projection.prior_generation.clone()),
            reason: match reason {
                SemanticRuntimeScheduleFailureV1::Artifact => {
                    SemanticFallbackReasonV1::ArtifactUnavailable
                }
                SemanticRuntimeScheduleFailureV1::Cancelled => {
                    SemanticFallbackReasonV1::RuntimeUnavailable
                }
                SemanticRuntimeScheduleFailureV1::Runtime
                | SemanticRuntimeScheduleFailureV1::Projection
                | SemanticRuntimeScheduleFailureV1::Publication => {
                    SemanticFallbackReasonV1::RuntimeFailure
                }
            },
        },
        SemanticRuntimeScheduleStatusV1::Current { generation } => {
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(generation.clone()),
                reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
            }
        }
    };
    SemanticRuntimeStatusV1::new(configuration, state)
}

/// Schedule `FastEmbed` projection for one published code generation.
///
/// Returns immediately after enqueueing; artifact load, model download, and
/// indexing run asynchronously and never join into ordinary search.
pub fn schedule_saved_code_generation<LoadArtifact, StageProjection, StageFuture>(
    handle: &DaemonSemanticRuntimeHandleV1,
    generation: &CodeIndexPublishedGenerationV1,
    load_artifact: LoadArtifact,
    stage_projection: StageProjection,
) -> bool
where
    LoadArtifact: FnOnce() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
        + Send
        + 'static,
    StageProjection: FnOnce(PreparedVectorGenerationV1) -> StageFuture + Send + 'static,
    StageFuture: Future<Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>>
        + Send
        + 'static,
{
    let Ok(request) = FastEmbedSemanticGenerationRequestV1::new(
        generation.manifest().generation_id.clone(),
        generation.projection().request().clone(),
        generation.chunks().chunks().to_vec(),
        load_artifact,
        stage_projection,
    ) else {
        return false;
    };
    // Enqueue only — callers must not await download/index completion.
    handle.schedule_generation(request)
}

/// Daemon-owned production bridge from lifecycle-ready model bytes to the
/// persistent vector store and atomically current query runtime.
#[derive(Clone)]
pub struct ProductionSemanticRuntimeV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    database: Arc<Database>,
    code_index_store_root: PathBuf,
    lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    resources: SemanticResourceCeilings,
}

#[derive(Clone, Debug)]
pub struct SemanticCompatibleCurrentGenerationSnapshotV1 {
    pub executable: SemanticExecutableGenerationV1,
    pub vector_state_revision: i64,
    pub vector_generation_id: VectorGenerationIdV1,
}

pub struct SemanticVectorPublicationLeaseV1<'runtime> {
    _writer: tokio::sync::MutexGuard<'runtime, ()>,
}

impl ProductionSemanticRuntimeV1 {
    #[allow(dead_code)] // production semantic runtime mount — preserve authority surface
    pub fn new(
        handle: DaemonSemanticRuntimeHandleV1,
        database: Arc<Database>,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
        resources: SemanticResourceCeilings,
    ) -> Self {
        let code_index_store_root = database
            .database_path()
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("code-index-v1");
        Self::new_with_code_index_store_root(
            handle,
            database,
            code_index_store_root,
            lifecycle,
            resources,
        )
    }

    fn new_with_code_index_store_root(
        handle: DaemonSemanticRuntimeHandleV1,
        database: Arc<Database>,
        code_index_store_root: PathBuf,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
        resources: SemanticResourceCeilings,
    ) -> Self {
        Self {
            handle,
            database,
            code_index_store_root,
            lifecycle,
            resources,
        }
    }

    async fn retain_code_generations(
        &self,
        store: &DatabaseVectorGenerationStoreV1<'_>,
        inventory: &DatabaseLegacyVectorInventoryV1,
    ) -> Result<Option<CodeGenerationRetentionReceiptV1>, SemanticRuntimeScheduleFailureV1> {
        let snapshot = inventory
            .read_only_inventory()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let vector_readable_sources = snapshot.retained_readable_sources();
        let store_root = self.code_index_store_root.clone();
        let plan_root = store_root.clone();
        let planned_sources = vector_readable_sources.clone();
        let plan = tokio::task::spawn_blocking(move || {
            recover_code_generation_retention(&plan_root, &planned_sources)?;
            plan_code_generation_retention(
                &plan_root,
                &planned_sources,
                DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            )
        })
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        if plan.collectable_generations.is_empty() {
            return Ok(None);
        }

        // Hold the canonical vector writer lane while re-reading liveness and
        // unlinking candidates. A vector publication cannot begin naming a
        // previously unmarked source between the final mark check and sweep.
        let writer = self
            .database
            .begin_write_transaction("retain code-index generations")
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let current_sources = store
            .read_legacy_inventory()
            .await
            .and_then(|inventory| {
                inventory.read_only_inventory().map_err(|error| {
                    crate::store::vector_generations::VectorGenerationStoreErrorV1::Storage(
                        error.to_string(),
                    )
                })
            })
            .map(|inventory| inventory.retained_readable_sources())
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication);
        let result = match current_sources {
            Ok(current_sources) if current_sources == vector_readable_sources => {
                execute_code_generation_retention(
                    &store_root,
                    plan,
                    CodeGenerationRetentionModeV1::Apply,
                    now_micros(),
                )
                .map(|report| report.receipt)
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)
            }
            Ok(_) => Ok(None),
            Err(error) => Err(error),
        };
        writer
            .rollback()
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        result
    }

    /// Restore a compatible immutable generation after daemon restart.
    pub async fn restore_current(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<bool, SemanticRuntimeScheduleFailureV1> {
        let store = DatabaseVectorGenerationStoreV1::open(self.database.as_ref())
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let projection = LoadedSemanticArtifactV1::lifecycle_projection(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
        )?;
        let source_manifest_digest =
            semantic_source_manifest_digest(generation.projection().request());
        let mut active = store
            .active_generation_for(
                &projection,
                &generation.manifest().generation_id,
                source_manifest_digest,
            )
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        if active.is_none() {
            let replay_digest = semantic_projection_request(generation, &projection, None)?
                .changes
                .manifest_digest;
            if &replay_digest != source_manifest_digest {
                active = store
                    .active_generation_for(
                        &projection,
                        &generation.manifest().generation_id,
                        &replay_digest,
                    )
                    .await
                    .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
            }
        }
        let Some(active) = active else {
            return Ok(false);
        };
        let lifecycle = Arc::clone(&self.lifecycle);
        let manifest = generation.manifest().clone();
        let resources = self.resources;
        let artifact = tokio::task::spawn_blocking(move || {
            LoadedSemanticArtifactV1::from_lifecycle(&lifecycle, &manifest, resources)
        })
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??;
        if artifact.projection() != &projection {
            return Ok(false);
        }
        let pointer = SemanticGenerationPointerV1 {
            generation: active.generation_id().clone(),
            source_generation: active.source_generation().clone(),
            projection_key: active.projection_key().clone(),
        };
        let handle = self.handle.clone();
        let prepared =
            tokio::task::spawn_blocking(move || handle.prepare_restore(pointer, artifact))
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??;
        if !self.handle.commit_restore(prepared) {
            return Err(SemanticRuntimeScheduleFailureV1::Publication);
        }
        let _ = self.lifecycle.mark_ready();
        Ok(true)
    }

    /// Enqueue one saved code generation. Model verification, ORT startup,
    /// changed-chunk embedding, and database publication remain background work.
    #[allow(dead_code)] // production semantic runtime mount — preserve authority surface
    pub fn schedule_saved_generation(&self, generation: &CodeIndexPublishedGenerationV1) -> bool {
        self.schedule_saved_generation_inner(generation, None)
    }

    /// Rebuild legacy vector state from the current retained canonical code in
    /// scratch storage, then replace it under one database revision CAS.
    ///
    /// This runs only from the daemon's background semantic work lane. A crash,
    /// cancellation, model failure, or stale revision before the final swap
    /// leaves the prior state untouched; a committed receipt makes restart
    /// idempotent.
    async fn migrate_legacy_vectors_for_generation(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Option<LegacyVectorMigrationReceiptV1>, SemanticRuntimeScheduleFailureV1> {
        let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(self.database.as_ref())
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let inventory = store.read_legacy_inventory().await;
        if let Ok(inventory) = inventory.as_ref()
            && let Err(error) = self.retain_code_generations(&store, inventory).await
        {
            tracing::warn!(
                event = "code_generation_retention",
                outcome = "deferred",
                error = %format!("{error:?}"),
                "code-generation retention failed closed; semantic scheduling continues"
            );
        }
        if let Some(receipt) = store
            .completed_legacy_migration_receipt()
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?
        {
            return Ok(Some(receipt));
        }
        let inventory = inventory.map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let snapshot = inventory
            .read_only_inventory()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        if snapshot.entries.is_empty() {
            return Ok(None);
        }
        if cancelled() {
            return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
        }

        let lifecycle = Arc::clone(&self.lifecycle);
        let resources = self.resources;
        let generation = generation.clone();
        let generations_root = self.code_index_store_root.join("code-generations-v1");
        let inventory_for_prepare = inventory.clone();
        let cancelled_for_prepare = Arc::clone(&cancelled);
        let (replacement, transaction) = tokio::task::spawn_blocking(move || {
            if cancelled_for_prepare() {
                return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
            }
            let generations =
                load_retained_code_generations(&generations_root, &generation, &snapshot)?;
            let retained = retained_canonical_chunk_sets(&snapshot, |source| {
                Ok(generations
                    .get(source)
                    .map(|generation| generation.chunks().chunks().to_vec()))
            })?;
            let mut prepared = BTreeMap::new();
            for chunks in retained.iter().filter(|chunks| !chunks.chunks().is_empty()) {
                let retained_generation = generations
                    .get(chunks.source_generation())
                    .ok_or(SemanticRuntimeScheduleFailureV1::Projection)?;
                let artifact = LoadedSemanticArtifactV1::from_lifecycle(
                    &lifecycle,
                    retained_generation.manifest(),
                    resources,
                )?;
                let projection = artifact.projection().clone();
                let request = semantic_projection_request(retained_generation, &projection, None)?;
                let projection = prepare_semantic_evaluation_projection(
                    artifact,
                    request,
                    chunks.chunks(),
                    resources.max_concurrent_sessions as usize,
                    resources.max_resident_bytes,
                )?
                .prepared;
                if prepared
                    .insert(chunks.source_generation().clone(), projection)
                    .is_some()
                {
                    return Err(SemanticRuntimeScheduleFailureV1::Projection);
                }
            }
            if cancelled_for_prepare() {
                return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
            }

            let replacement =
                std::rc::Rc::new(std::cell::RefCell::new(FakeVectorGenerationStoreV1::new()));
            let prepared = std::rc::Rc::new(std::cell::RefCell::new(prepared));
            let replacement_for_stage = std::rc::Rc::clone(&replacement);
            let prepared_for_stage = std::rc::Rc::clone(&prepared);
            let mut rebuilder =
                ProductionLegacyVectorCanonicalRebuilderV1::try_new(retained, move |chunks| {
                    let prepared = prepared_for_stage
                        .borrow_mut()
                        .remove(chunks.source_generation())
                        .ok_or(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch)?;
                    if prepared.request.changes.to_generation != *chunks.source_generation()
                        || prepared.request.changes.added_or_changed.len() != chunks.chunks().len()
                    {
                        return Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch);
                    }
                    let plan = VectorGenerationPlanV1 {
                        target_projection_key: prepared.request.target_projection_key.clone(),
                        source_generation: prepared.request.changes.to_generation.clone(),
                        source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                        expected_chunk_ids: chunks
                            .chunks()
                            .iter()
                            .map(|chunk| chunk.id.clone())
                            .collect(),
                        base_generation: None,
                    };
                    let mut replacement = replacement_for_stage.borrow_mut();
                    let build = replacement
                        .rebuild_generation(plan)
                        .map_err(map_legacy_store)?;
                    replacement
                        .commit_batch(&build, None, prepared)
                        .map_err(map_legacy_store)?;
                    let publication = replacement
                        .seal_generation_inactive(&build)
                        .map_err(map_legacy_store)?;
                    Ok(StagedCanonicalVectorRebuildV1 {
                        source_generation: chunks.source_generation().clone(),
                        rebuilt_generation: publication.generation_id,
                        canonical_chunk_set_digest: chunks.digest().clone(),
                    })
                })
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
            let transaction = prepare_legacy_vector_migration(
                &inventory_for_prepare,
                &mut rebuilder,
                &NeverCancelLegacyVectorMigrationV1,
            )
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
            drop(rebuilder);
            let replacement = std::rc::Rc::try_unwrap(replacement)
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
                .into_inner();
            Ok((replacement, transaction))
        })
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??;
        let receipt = replace_legacy_vectors_after_rebuild(
            &store,
            &inventory,
            replacement,
            &transaction,
            cancelled.as_ref(),
        )
        .await?;
        Ok(Some(receipt))
    }

    /// Build an evaluator-only exact-flat lane from the checked-in sanitized
    /// corpus. The verified production artifact/runtime are reused, while the
    /// resulting vectors remain process-local and cannot replace the project's
    /// active vector generation.
    pub fn prepare_evaluation_generation(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<PreparedSemanticEvaluationGenerationV1, SemanticRuntimeScheduleFailureV1> {
        let model_bytes = installed_artifact_member_bytes(&self.lifecycle)?;
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
        )?;
        let artifact_digest = artifact
            .projection()
            .embedding_key()
            .model_artifact_digest
            .clone();
        let projection = artifact.projection().clone();
        let request = semantic_projection_request(generation, &projection, None)?;
        let projection_input_bytes = projection_input_bytes(generation.chunks().chunks())?;
        let started = std::time::Instant::now();
        let prepared = prepare_semantic_evaluation_projection(
            artifact,
            request,
            generation.chunks().chunks(),
            self.resources.max_concurrent_sessions as usize,
            self.resources.max_resident_bytes,
        )?;
        PreparedSemanticEvaluationGenerationV1::new(
            generation.clone(),
            prepared,
            artifact_digest,
            model_bytes,
            elapsed_micros(started),
            projection_input_bytes,
        )
    }

    /// Measure a genuine incremental evaluator projection from an already
    /// prepared immutable generation. This never publishes a durable pointer
    /// or relabels a clean rebuild.
    pub fn measure_incremental_evaluation_projection(
        &self,
        current: &PreparedSemanticEvaluationGenerationV1,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<ProductionCandidateNativeGenerationResourcesV1, SemanticRuntimeScheduleFailureV1>
    {
        let request = semantic_projection_request(
            generation,
            &current.projection,
            Some(&SemanticGenerationPointerV1 {
                generation: current.vector_generation.clone(),
                source_generation: current.source_generation.clone(),
                projection_key: current.projection.projection_key().clone(),
            }),
        )?;
        if request.changes.from_generation.as_ref() != Some(&current.source_generation)
            || request.changes.added_or_changed.is_empty()
            || request.changes.added_or_changed.len() >= generation.chunks().chunks().len()
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let changed = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
        )?;
        let started = std::time::Instant::now();
        let prepared = prepare_semantic_evaluation_projection(
            artifact,
            request,
            &chunks,
            self.resources.max_concurrent_sessions as usize,
            self.resources.max_resident_bytes,
        )?;
        if prepared.prepared.request.changes.from_generation.as_ref()
            != Some(&current.source_generation)
            || prepared.prepared.request.changes.to_generation
                != generation.manifest().generation_id
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let mut resources = current.generation_resources();
        resources.incremental_source_generation = generation.manifest().generation_id.clone();
        resources.incremental_source_manifest_digest =
            prepared.prepared.request.changes.manifest_digest.clone();
        resources.incremental_rebuild_micros = elapsed_micros(started);
        Ok(resources)
    }

    pub fn measure_evaluation_projection_cases(
        &self,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        block_on_semantic_evaluation(
            self.measure_evaluation_projection_cases_sqlite(clean, sources),
        )
    }

    async fn measure_evaluation_projection_cases_sqlite(
        &self,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        let store =
            DatabaseVectorEvaluationStoreV1::open(self.database.as_ref(), evaluation_state_id())
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let measured = self
            .measure_evaluation_projection_cases_in_store(&store, clean, sources)
            .await;
        let closed = store
            .close()
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication);
        match (measured, closed) {
            (Ok(samples), Ok(())) => Ok(samples),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn measure_evaluation_projection_cases_in_store(
        &self,
        store: &DatabaseVectorEvaluationStoreV1<'_>,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        let clean_prepared = &clean.prepared_projection;
        if clean_prepared.request.changes.to_generation != clean.source_generation
            || clean_prepared.request.changes.from_generation.is_some()
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let clean_plan = VectorGenerationPlanV1 {
            target_projection_key: clean_prepared.request.target_projection_key.clone(),
            source_generation: clean_prepared.request.changes.to_generation.clone(),
            source_manifest_digest: clean_prepared.request.changes.manifest_digest.clone(),
            expected_chunk_ids: clean_prepared
                .vectors
                .iter()
                .map(|vector| vector.chunk_id.clone())
                .collect(),
            base_generation: None,
        };
        let clean_build = store
            .rebuild_generation(clean_plan)
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        let clean_checkpoint = store
            .commit_batch(&clean_build, None, clean_prepared.clone())
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        let clean_publication = store
            .publish_generation(&clean_build, None)
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        if store
            .active_generation_id()
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
            != Some(clean_publication.generation_id.clone())
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }

        let mut samples = BTreeMap::new();
        samples.insert(
            SemanticProjectionCaseV1::Clean,
            projection_case_sample_from_prepared(
                clean_prepared,
                clean.resources.clean_projection_build_micros,
                clean.projection_input_bytes,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let replay_started = std::time::Instant::now();
        let replay_checkpoint = store
            .commit_batch(
                &clean_build,
                Some(&clean_checkpoint),
                clean_prepared.clone(),
            )
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        if replay_checkpoint != clean_checkpoint
            || store
                .active_generation_id()
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
                != Some(clean_publication.generation_id.clone())
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        samples.insert(
            SemanticProjectionCaseV1::IdempotencyReplay,
            SemanticProjectionCaseSampleV1 {
                outcome: SemanticProjectionCaseOutcomeV1::Complete,
                elapsed_micros: elapsed_micros(replay_started),
                input_bytes: 0,
                chunks_added_or_changed: 0,
                chunks_deleted: 0,
                chunks_reused: 0,
                projection_calls: 0,
            },
        );

        let clean_pointer = SemanticGenerationPointerV1 {
            generation: clean_publication.generation_id.clone(),
            source_generation: clean_prepared.request.changes.to_generation.clone(),
            projection_key: clean_prepared.request.target_projection_key.clone(),
        };
        let (one_symbol, one_symbol_elapsed, one_symbol_input) =
            self.prepare_projection_case(sources.one_symbol, Some(&clean_pointer))?;
        let one_symbol_publication = publish_evaluation_projection_case_sqlite(
            store,
            sources.one_symbol,
            one_symbol.clone(),
            Some(clean_publication.generation_id.clone()),
            Some(&clean_publication.generation_id),
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::OneSymbol,
            projection_case_sample_from_prepared(
                &one_symbol,
                one_symbol_elapsed,
                one_symbol_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let one_symbol_pointer = SemanticGenerationPointerV1 {
            generation: one_symbol_publication.generation_id.clone(),
            source_generation: one_symbol.request.changes.to_generation.clone(),
            projection_key: one_symbol.request.target_projection_key.clone(),
        };
        let (no_op, no_op_elapsed, no_op_input) =
            self.prepare_projection_case(sources.no_op, Some(&one_symbol_pointer))?;
        let no_op_publication = publish_evaluation_projection_case_sqlite(
            store,
            sources.no_op,
            no_op.clone(),
            Some(one_symbol_publication.generation_id.clone()),
            Some(&one_symbol_publication.generation_id),
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::NoOp,
            projection_case_sample_from_prepared(
                &no_op,
                no_op_elapsed,
                no_op_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let no_op_pointer = SemanticGenerationPointerV1 {
            generation: no_op_publication.generation_id.clone(),
            source_generation: no_op.request.changes.to_generation.clone(),
            projection_key: no_op.request.target_projection_key.clone(),
        };
        let (deletion, deletion_elapsed, deletion_input) =
            self.prepare_projection_case(sources.deletion, Some(&no_op_pointer))?;
        let deletion_publication = publish_evaluation_projection_case_sqlite(
            store,
            sources.deletion,
            deletion.clone(),
            Some(no_op_publication.generation_id.clone()),
            Some(&no_op_publication.generation_id),
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::Deletion,
            projection_case_sample_from_prepared(
                &deletion,
                deletion_elapsed,
                deletion_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let active_before_cancellation = deletion_publication.generation_id.clone();
        let cancellation_started = std::time::Instant::now();
        let cancellation_plan = evaluation_projection_plan(
            sources.deletion,
            &deletion,
            Some(no_op_publication.generation_id.clone()),
        )?;
        let cancellation_build = store
            .rebuild_generation(cancellation_plan)
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        if !store
            .cancel_generation(&cancellation_build)
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
            || store
                .active_generation_id()
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
                != Some(active_before_cancellation.clone())
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        samples.insert(
            SemanticProjectionCaseV1::Cancellation,
            SemanticProjectionCaseSampleV1 {
                outcome: SemanticProjectionCaseOutcomeV1::CancelledWithoutPublication,
                elapsed_micros: elapsed_micros(cancellation_started),
                input_bytes: 0,
                chunks_added_or_changed: 0,
                chunks_deleted: 0,
                chunks_reused: 0,
                projection_calls: 0,
            },
        );

        let (incompatible, incompatible_elapsed, incompatible_input) =
            self.prepare_projection_case(sources.one_symbol, None)?;
        if incompatible.request.changes.from_generation.is_some()
            || incompatible.request.previous_projection_key.is_some()
            || incompatible.request.replay_reason
                != ProjectionReplayReasonV1::FullRebuildIncompatible
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let incompatible_plan =
            evaluation_projection_plan(sources.one_symbol, &incompatible, None)?;
        let incompatible_build = store
            .rebuild_generation(incompatible_plan)
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        store
            .commit_batch(&incompatible_build, None, incompatible.clone())
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        if store
            .active_generation_for(
                &incompatible.embedding_key,
                &incompatible.request.changes.to_generation,
                &incompatible.request.changes.manifest_digest,
            )
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
            .is_some()
            || !store
                .cancel_generation(&incompatible_build)
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
            || store
                .active_generation_id()
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
                != Some(active_before_cancellation)
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        samples.insert(
            SemanticProjectionCaseV1::IncompatibleState,
            projection_case_sample_from_prepared(
                &incompatible,
                incompatible_elapsed,
                incompatible_input,
                SemanticProjectionCaseOutcomeV1::FullRebuildIncompatible,
            ),
        );
        Ok(samples)
    }

    fn prepare_projection_case(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        current: Option<&SemanticGenerationPointerV1>,
    ) -> Result<(PreparedVectorGenerationV1, u64, u64), SemanticRuntimeScheduleFailureV1> {
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
        )?;
        let projection = artifact.projection().clone();
        let request = semantic_projection_request(generation, &projection, current)?;
        let changed = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        if chunks.len() != request.changes.added_or_changed.len() {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let input_bytes = projection_input_bytes(&chunks)?;
        let started = std::time::Instant::now();
        let prepared = prepare_semantic_evaluation_projection(
            artifact,
            request,
            &chunks,
            self.resources.max_concurrent_sessions as usize,
            self.resources.max_resident_bytes,
        )?;
        Ok((prepared.prepared, elapsed_micros(started), input_bytes))
    }

    #[allow(dead_code)] // production semantic runtime mount — preserve authority surface
    pub(crate) async fn inspect_compatible_current_generation(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<SemanticExecutableGenerationV1, SemanticRuntimeBackendErrorV1> {
        Ok(self
            .inspect_compatible_current_generation_snapshot(
                required,
                source_generation,
                source_manifest_digest,
            )
            .await?
            .executable)
    }

    pub async fn inspect_compatible_current_generation_snapshot(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<SemanticCompatibleCurrentGenerationSnapshotV1, SemanticRuntimeBackendErrorV1> {
        let active = DatabaseVectorGenerationStoreV1::read_active_generation_snapshot_for(
            self.database.as_ref(),
            &required.projection,
            source_generation,
            source_manifest_digest,
        )
        .await
        .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
        let active = active.ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
        if active.generation().generation_id() != &required.vector_generation_id {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        let pointer = self
            .handle
            .current()
            .ok_or(SemanticRuntimeBackendErrorV1::Unavailable)?;
        if pointer.generation != required.vector_generation_id
            || pointer.source_generation != *source_generation
            || pointer.projection_key != *required.projection.projection_key()
        {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        let executable = self.inspect_generation(required).await?;
        if !DatabaseVectorGenerationStoreV1::active_snapshot_is_current(
            self.database.as_ref(),
            active.revision(),
            active.generation().generation_id(),
        )
        .await
        .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
        {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        Ok(SemanticCompatibleCurrentGenerationSnapshotV1 {
            executable,
            vector_state_revision: active.revision(),
            vector_generation_id: active.generation().generation_id().clone(),
        })
    }

    /// Freeze vector-pointer mutation while a freshness-bound accepted profile
    /// publication commits. Every vector mutation enters this same writer
    /// lane, so a validated revision/generation remains exact for the lease.
    pub async fn acquire_vector_publication_lease(
        &self,
        expected_revision: i64,
        expected_generation: &VectorGenerationIdV1,
    ) -> Result<SemanticVectorPublicationLeaseV1<'_>, SemanticRuntimeBackendErrorV1> {
        let writer = self.database.writer().await;
        if !DatabaseVectorGenerationStoreV1::active_snapshot_is_current(
            self.database.as_ref(),
            expected_revision,
            expected_generation,
        )
        .await
        .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
        {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        Ok(SemanticVectorPublicationLeaseV1 { _writer: writer })
    }

    fn schedule_saved_generation_fair(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        lease: SemanticProjectionLeaseV1,
    ) -> bool {
        self.schedule_saved_generation_inner(generation, Some(lease))
    }

    fn schedule_saved_generation_inner(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        fair_lease: Option<SemanticProjectionLeaseV1>,
    ) -> bool {
        let projection = match LoadedSemanticArtifactV1::lifecycle_projection(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
        ) {
            Ok(projection) => projection,
            Err(_) => {
                return schedule_saved_code_generation(
                    &self.handle,
                    generation,
                    || Err(SemanticRuntimeScheduleFailureV1::Artifact),
                    move |_prepared| async move {
                        drop(fair_lease);
                        Err(SemanticRuntimeScheduleFailureV1::Publication)
                    },
                );
            }
        };
        let current = self.handle.current();
        let request = match semantic_projection_request(generation, &projection, current.as_ref()) {
            Ok(request) => request,
            Err(_) => return false,
        };
        let changed_ids = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<std::collections::BTreeSet<_>>();
        let canonical_chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed_ids.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        let target_generation = generation.manifest().generation_id.clone();
        let expected_chunk_ids = generation
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();
        let base_generation = current.as_ref().and_then(|pointer| {
            (request.changes.from_generation.as_ref() == Some(&pointer.source_generation)
                && request.previous_projection_key.as_ref() == Some(&pointer.projection_key))
            .then(|| pointer.generation.clone())
        });
        let expected_active = base_generation.clone();
        let database = Arc::clone(&self.database);
        let lifecycle_for_load = Arc::clone(&self.lifecycle);
        let lifecycle_for_stage = Arc::clone(&self.lifecycle);
        let lifecycle_for_commit = Arc::clone(&self.lifecycle);
        let manifest = generation.manifest().clone();
        let resources = self.resources;
        let total_units = request.changes.added_or_changed.len().max(1) as u64;
        let _ = self.lifecycle.mark_loading();
        let _ = self.lifecycle.mark_indexing(0, total_units);
        let request = match FastEmbedSemanticGenerationRequestV1::new(
            target_generation,
            request,
            canonical_chunks,
            move || {
                LoadedSemanticArtifactV1::from_lifecycle(&lifecycle_for_load, &manifest, resources)
            },
            move |prepared| async move {
                if fair_lease
                    .as_ref()
                    .is_some_and(SemanticProjectionLeaseV1::is_cancelled)
                {
                    return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
                }
                let store = DatabaseVectorGenerationStoreV1::open(database.as_ref())
                    .await
                    .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                let published_source_generation = prepared.request.changes.to_generation.clone();
                let published_projection_key = prepared.request.target_projection_key.clone();
                let plan = VectorGenerationPlanV1 {
                    target_projection_key: published_projection_key.clone(),
                    source_generation: published_source_generation.clone(),
                    source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                    expected_chunk_ids,
                    base_generation: base_generation.clone(),
                };
                let build = store
                    .begin_generation(plan)
                    .await
                    .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                store
                    .commit_batch(&build, None, prepared)
                    .await
                    .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                let _ = lifecycle_for_stage.mark_indexing(total_units, total_units);
                let _ = store;
                let database_for_commit = Arc::clone(&database);
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    let _publication_lease = fair_lease
                        .as_ref()
                        .map(SemanticProjectionLeaseV1::try_begin_publication)
                        .transpose()
                        .map_err(fair_schedule_failure)?;
                    let store = DatabaseVectorGenerationStoreV1::open(database_for_commit.as_ref())
                        .await
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                    let publication = store
                        .publish_generation(&build, expected_active.as_ref())
                        .await
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                    let _ = lifecycle_for_commit.mark_ready();
                    Ok(SemanticGenerationPointerV1 {
                        generation: publication.generation_id,
                        source_generation: published_source_generation,
                        projection_key: published_projection_key,
                    })
                }))
            },
        ) {
            Ok(request) => request,
            Err(_) => return false,
        };
        let scheduled = self.handle.schedule_generation(request);
        if scheduled {
            let handle = self.handle.clone();
            let lifecycle = Arc::clone(&self.lifecycle);
            tokio::spawn(async move {
                loop {
                    match handle.status() {
                        SemanticRuntimeScheduleStatusV1::Indexing {
                            completed_units,
                            total_units,
                            ..
                        } => {
                            let _ = lifecycle.mark_indexing(completed_units, total_units);
                        }
                        SemanticRuntimeScheduleStatusV1::Current { .. } => {
                            let _ = lifecycle.mark_ready();
                            break;
                        }
                        SemanticRuntimeScheduleStatusV1::Failed { reason, .. } => {
                            let _ = lifecycle
                                .mark_runtime_failed(format!("semantic runtime {reason:?}"), true);
                            break;
                        }
                        SemanticRuntimeScheduleStatusV1::Unavailable => break,
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            });
        }
        scheduled
    }

    /// Real application consumer for the optional semantic lane. The durable
    /// active generation is loaded before composition; indexing/download never
    /// enters this request path.
    pub async fn execute_search<C>(
        &self,
        code_generation: &CodeIndexPublishedGenerationV1,
        request: &SemanticRetrievalRequestV1<'_>,
        calibration: Option<&SemanticCalibrationProfileV1>,
        control: &C,
        mode: SemanticQueryModeV1,
        fallback: Arc<QueryFallbackSubpayload>,
    ) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
    where
        C: SemanticExecutionControl + Sync,
    {
        let source_manifest_digest =
            semantic_source_manifest_digest(code_generation.projection().request());
        let mut active = match DatabaseVectorGenerationStoreV1::read_active_generation_for(
            self.database.as_ref(),
            request.projection,
            &code_generation.manifest().generation_id,
            source_manifest_digest,
        )
        .await
        {
            Ok(active) => active,
            Err(_) => {
                return execute_calibrated_semantic_query(
                    &NeverCalledSemanticLane,
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Failed),
                    mode,
                    fallback,
                );
            }
        };
        if active.is_none() {
            let replay_digest =
                semantic_projection_request(code_generation, request.projection, None)
                    .map_err(|_| SemanticQueryServiceError::InvalidFallback)?
                    .changes
                    .manifest_digest;
            if &replay_digest != source_manifest_digest {
                active = DatabaseVectorGenerationStoreV1::read_active_generation_for(
                    self.database.as_ref(),
                    request.projection,
                    &code_generation.manifest().generation_id,
                    &replay_digest,
                )
                .await
                .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
            }
        }
        let Some(active) = active else {
            return execute_calibrated_semantic_query(
                &NeverCalledSemanticLane,
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        };
        let complete = CompleteSemanticGenerationV1::new(
            active.projection_key().clone(),
            request.search_index_key.clone(),
            active.generation_id().clone(),
            active.source_generation().clone(),
            code_generation.capability().manifest_digest.clone(),
        )
        .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
        let vectors = PublishedSemanticVectorReadPortV1::new(
            active,
            request.search_index_key.clone(),
            code_generation,
        )
        .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
        compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
            handle: &self.handle,
            request,
            generation: &complete,
            calibration,
            vectors: &vectors,
            control,
            mode,
            fallback,
        })
    }

    pub async fn rollback(
        &self,
        target: &VectorGenerationIdV1,
        expected_active: &VectorGenerationIdV1,
    ) -> Result<SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1> {
        let store = DatabaseVectorGenerationStoreV1::open(self.database.as_ref())
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let generation =
            DatabaseVectorGenerationStoreV1::read_generation(self.database.as_ref(), target)
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?
                .ok_or(SemanticRuntimeScheduleFailureV1::Publication)?;
        let lifecycle = Arc::clone(&self.lifecycle);
        let projection = generation.embedding_key().clone();
        let resources = self.resources;
        let mut artifact = tokio::task::spawn_blocking(move || {
            LoadedSemanticArtifactV1::from_lifecycle_projection(&lifecycle, &projection, resources)
        })
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        if artifact.is_err() && self.lifecycle.rollback_to_previous().is_ok() {
            let lifecycle = Arc::clone(&self.lifecycle);
            let projection = generation.embedding_key().clone();
            let resources = self.resources;
            artifact = tokio::task::spawn_blocking(move || {
                LoadedSemanticArtifactV1::from_lifecycle_projection(
                    &lifecycle,
                    &projection,
                    resources,
                )
            })
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        }
        let artifact = artifact?;
        let pointer = SemanticGenerationPointerV1 {
            generation: target.clone(),
            source_generation: generation.source_generation().clone(),
            projection_key: generation.projection_key().clone(),
        };
        let handle = self.handle.clone();
        let prepared_pointer = pointer.clone();
        let prepared_runtime =
            tokio::task::spawn_blocking(move || handle.prepare_restore(prepared_pointer, artifact))
                .await
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??;
        let publication = store
            .activate_generation(target, Some(expected_active))
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        debug_assert_eq!(publication.generation_id, pointer.generation);
        if !self.handle.commit_restore(prepared_runtime) {
            return Err(SemanticRuntimeScheduleFailureV1::Publication);
        }
        let _ = self.lifecycle.mark_ready();
        Ok(pointer)
    }
}

pub struct PreparedSemanticEvaluationGenerationV1 {
    source_generation: CodeGenerationId,
    projection: tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    search_index_key: SemanticSearchIndexKeyV1,
    vector_generation: VectorGenerationIdV1,
    prepared_projection: PreparedVectorGenerationV1,
    projection_input_bytes: u64,
    capability_manifest_digest: ManifestDigest,
    query_factory: DaemonSemanticQueryFactoryV1,
    vectors: PublishedSemanticVectorReadPortV1,
    query_keys: RetrievalCursorKeyringV1,
    resources: ProductionCandidateNativeGenerationResourcesV1,
}

impl PreparedSemanticEvaluationGenerationV1 {
    fn new(
        code: CodeIndexPublishedGenerationV1,
        prepared: PreparedSemanticEvaluationProjectionV1,
        artifact_digest: ManifestDigest,
        model_bytes: u64,
        clean_projection_build_micros: u64,
        projection_input_bytes: u64,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let vector_digest = canonical_sha256(&(
            "tracedecay.semantic-evaluation-vector-generation.v1",
            &prepared.prepared.embedding_key,
            &prepared.prepared.request,
            &prepared.prepared.receipt,
            prepared
                .prepared
                .vectors
                .iter()
                .map(|vector| &vector.output_digest)
                .collect::<Vec<_>>(),
        ))
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        let vector_generation = VectorGenerationIdV1::new(vector_digest);
        let vector_bytes = prepared
            .prepared
            .vectors
            .iter()
            .try_fold(0_u64, |total, vector| {
                let bytes = u64::try_from(vector.values.len())
                    .ok()?
                    .checked_mul(std::mem::size_of::<f32>() as u64)?;
                total.checked_add(bytes)
            })
            .ok_or(SemanticRuntimeScheduleFailureV1::Projection)?;
        let source_manifest_digest = prepared.prepared.request.changes.manifest_digest.clone();
        let source_generation = code.manifest().generation_id.clone();
        let resources = ProductionCandidateNativeGenerationResourcesV1 {
            source_generation: source_generation.clone(),
            source_manifest_digest,
            incremental_source_generation: source_generation.clone(),
            incremental_source_manifest_digest: prepared
                .prepared
                .request
                .changes
                .manifest_digest
                .clone(),
            vector_generation: Some(vector_generation.clone()),
            artifact_digest: Some(artifact_digest),
            model_bytes,
            vector_bytes,
            index_bytes: 0,
            cache_bytes: 0,
            clean_projection_build_micros,
            incremental_rebuild_micros: 0,
            projection_cases: BTreeMap::new(),
        };
        let projection = prepared.prepared.embedding_key.clone();
        let search_index_key = SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        let vectors = PublishedSemanticVectorReadPortV1::from_prepared(
            &prepared.prepared,
            vector_generation.clone(),
            search_index_key.clone(),
            &code,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        let secret = canonical_sha256(&(
            "tracedecay.semantic-evaluation-query-key.v1",
            code.manifest().generation_id.clone(),
            vector_generation.clone(),
        ))
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?
        .as_str()
        .as_bytes()
        .to_vec();
        let query_keys = RetrievalCursorKeyringV1::new(
            projection.privacy_domain().clone(),
            RetrievalCursorKeyId::new("semantic-evaluation.query-key.v1")
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?,
            projection.privacy_key_epoch(),
            secret,
            60_000_000,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        Ok(Self {
            source_generation,
            projection,
            search_index_key,
            vector_generation,
            prepared_projection: prepared.prepared,
            projection_input_bytes,
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            query_factory: prepared.query_factory,
            vectors,
            query_keys,
            resources,
        })
    }

    pub(crate) fn generation_resources(&self) -> ProductionCandidateNativeGenerationResourcesV1 {
        let mut resources = self.resources.clone();
        resources.cache_bytes = self.query_factory.resident_cache_bytes();
        resources
    }

    pub fn projection(&self) -> &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
        &self.projection
    }

    pub fn with_query_inputs(
        &self,
        context: ProductionCandidateNativeQueryContextV1<'_>,
        rerank_authority: Option<&ProductionCodeRerankAuthorityV1>,
        evaluate: &mut dyn for<'inputs> FnMut(
            ProductionCandidateNativeQueryInputsV1<'inputs>,
        ) -> Result<(), CandidateOutputError>,
    ) -> Result<(), CandidateOutputError> {
        if context.code_generation != &self.source_generation
            || context.code.manifest().generation_id != self.source_generation
        {
            return Err(CandidateOutputError::Contract(
                "native semantic evaluator generation changed".to_owned(),
            ));
        }
        let control = SemanticEvaluationExecutionControlV1 {
            started: std::time::Instant::now(),
        };
        let mut rerank_views =
            GenerationBoundCodeRerankViewsV1::new(context.code, context.query_view);
        let rerank = context
            .rerank_policy
            .zip(rerank_authority)
            .map(|(policy, authority)| {
                tracedecay_search_eval::semantic_native::SemanticNativeRerankInputV1 {
                    request: context.request,
                    policy,
                    views: &mut rerank_views as &mut _,
                    executor: authority.executor(),
                    control: &control,
                }
            });
        if context.profile.semantic_weight_ppm == 0 {
            return evaluate(ProductionCandidateNativeQueryInputsV1 {
                semantic: None,
                rerank,
            });
        }
        let query_digest = self
            .query_keys
            .digest_active_query(context.request, context.query_view)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        let request = SemanticRetrievalRequestV1 {
            base: context.request.clone(),
            query_digest,
            query_view: context.query_view,
            projection: &self.projection,
            search_index_key: &self.search_index_key,
            capability_manifest_digest: self.capability_manifest_digest.clone(),
            vector_generation: self.vector_generation.clone(),
            code_generation: self.source_generation.clone(),
            budget: context.request.budget,
        };
        request
            .validate()
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        let embedder = self.query_factory.create(&control);
        let scoped_vectors = ScopedSemanticEvaluationVectorReadPortV1 {
            inner: &self.vectors,
            allowed_chunks: context.semantic_allowed_chunks,
        };
        let lane = SemanticCodeRetriever::new(&embedder, &scoped_vectors, &control);
        evaluate(ProductionCandidateNativeQueryInputsV1 {
            semantic: Some(
                tracedecay_search_eval::semantic_native::SemanticNativeSemanticInputV1 {
                    lane: &lane,
                    request: &request,
                },
            ),
            rerank,
        })
    }
}

struct SemanticEvaluationExecutionControlV1 {
    started: std::time::Instant,
}

impl SemanticExecutionControl for SemanticEvaluationExecutionControlV1 {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn elapsed_micros(&self) -> u64 {
        self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }
}

impl RerankExecutionControlV1 for SemanticEvaluationExecutionControlV1 {
    fn elapsed_micros(&self) -> u64 {
        self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

impl SemanticRuntimeGenerationInspectorV1 for ProductionSemanticRuntimeV1 {
    fn inspect_generation<'a>(
        &'a self,
        required: &'a crate::config::retrieval::SemanticCompatibilityPinsV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticExecutableGenerationV1, SemanticRuntimeBackendErrorV1>,
    > {
        Box::pin(async move {
            let generation = DatabaseVectorGenerationStoreV1::read_generation(
                self.database.as_ref(),
                &required.vector_generation_id,
            )
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
            let lifecycle = Arc::clone(&self.lifecycle);
            let projection = generation.embedding_key().clone();
            let resources = self.resources;
            let verified = tokio::task::spawn_blocking(move || {
                LoadedSemanticArtifactV1::from_lifecycle_projection(
                    &lifecycle,
                    &projection,
                    resources,
                )
            })
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            if verified.projection() != generation.embedding_key()
                || required.projection != *generation.embedding_key()
                || required.implementation_revision.as_str() != "semantic.fastembed.production.v1"
            {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            let lifecycle = self.lifecycle.status();
            let state = lifecycle
                .state
                .ok_or(SemanticRuntimeBackendErrorV1::Unavailable)?;
            let artifact_digest = state.artifact_digest();
            let expected_artifact = required.artifact_manifest_digest.as_str();
            if artifact_digest != expected_artifact
                && expected_artifact.strip_prefix("sha256:") != Some(artifact_digest)
            {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            let expected_runtime_digest = canonical_sha256(&(
                "tracedecay.semantic-runtime-compatibility.v1",
                &generation.embedding_key().embedding_key().runtime_backend,
                &generation
                    .embedding_key()
                    .embedding_key()
                    .runtime_build_revision,
                generation.embedding_key().embedding_key().device_class,
                generation.embedding_key().embedding_key().precision,
            ))
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            if required.runtime_compatibility_digest != expected_runtime_digest {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            SemanticExecutableGenerationV1::new(
                required.clone(),
                crate::config::retrieval::SemanticResourceRequirementV1 {
                    model_bytes: self.resources.max_model_bytes,
                    tokenizer_bytes: self.resources.max_tokenizer_bytes,
                    resident_bytes: self.resources.max_resident_bytes,
                    threads: self.resources.max_threads,
                    batch_size: self.resources.max_batch_size,
                    sequence_length: self.resources.max_sequence_length,
                    load_deadline_ms: self.resources.load_deadline_ms,
                },
                true,
                true,
            )
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
        })
    }
}

fn semantic_source_manifest_digest(request: &ProjectionBatchRequestV1) -> &ManifestDigest {
    &request.changes.manifest_digest
}

fn installed_artifact_member_bytes(
    lifecycle: &SemanticModelLifecycleOwnerV1,
) -> Result<u64, SemanticRuntimeScheduleFailureV1> {
    let status = lifecycle.status();
    let state = status
        .state
        .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
    let model = lifecycle
        .catalog()
        .get(state.model_id())
        .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
    model
        .members
        .get("model")
        .map(|member| member.length)
        .filter(|bytes| *bytes != 0)
        .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)
}

fn evaluation_state_id() -> String {
    static NEXT_EVALUATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let sequence = NEXT_EVALUATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "semantic-native-evaluation:{}:{sequence}",
        std::process::id()
    )
}

fn block_on_semantic_evaluation<Output>(
    future: impl Future<Output = Result<Output, SemanticRuntimeScheduleFailureV1>>,
) -> Result<Output, SemanticRuntimeScheduleFailureV1> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(future),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?
            .block_on(future),
    }
}

fn elapsed_micros(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

struct PublishedSemanticVectorReadPortV1 {
    generation: VectorGenerationIdV1,
    projection_key: tracedecay_domain::ProjectionKeyV1,
    search_index_key: SemanticSearchIndexKeyV1,
    source_generation: CodeGenerationId,
    capability_manifest_digest: ManifestDigest,
    rows: Vec<SemanticVectorRecordV1>,
}

fn semantic_candidate_identity(
    chunk: &CodeSearchChunkV1,
) -> Result<(RetrievalAnchorId, LogicalEvidenceId, SourceOccurrenceId), RetrievalPortError> {
    let chunk_id = chunk.id.as_str();
    let evidence_id = chunk.anchor.symbol_occurrence_id.as_ref().map_or_else(
        || format!("code-chunk:{chunk_id}"),
        |symbol| format!("code-symbol:{}", symbol.as_str()),
    );
    Ok((
        RetrievalAnchorId::new(evidence_id.clone())
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        LogicalEvidenceId::new(evidence_id)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        SourceOccurrenceId::new(format!("code-chunk:{chunk_id}"))
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
    ))
}

struct ScopedSemanticEvaluationVectorReadPortV1<'a> {
    inner: &'a PublishedSemanticVectorReadPortV1,
    allowed_chunks: &'a BTreeSet<tracedecay_domain::CodeSearchChunkId>,
}

impl SemanticVectorReadPort for ScopedSemanticEvaluationVectorReadPortV1<'_> {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        let mut eligible = 0_u64;
        let mut scoped_visit = |row: &SemanticVectorRecordV1| {
            if self.allowed_chunks.contains(&row.chunk_id) {
                eligible = eligible.saturating_add(1);
                visit(row)?;
            }
            Ok(())
        };
        let summary = self.inner.scan_exact_flat(request, &mut scoped_visit)?;
        Ok(SemanticVectorScanSummaryV1 {
            examined: summary.examined,
            eligible,
            excluded: summary.examined.saturating_sub(eligible),
            unknown: summary.unknown,
        })
    }
}

impl PublishedSemanticVectorReadPortV1 {
    fn from_prepared(
        prepared: &PreparedVectorGenerationV1,
        generation: VectorGenerationIdV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
    ) -> Result<Self, RetrievalPortError> {
        if prepared.request.changes.to_generation != code.manifest().generation_id {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        let freshness = production_code_index_freshness(
            code.manifest().seal.sealed_at,
            ComponentRevision::new("policy.semantic.evaluation.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let chunks = code
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (&chunk.id, chunk))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::with_capacity(prepared.vectors.len());
        for (ordinal, vector) in prepared.vectors.iter().enumerate() {
            let chunk = chunks
                .get(&vector.chunk_id)
                .ok_or(RetrievalPortError::GenerationMismatch)?;
            let chunk_id = &vector.chunk_id;
            let (anchor_id, logical_evidence_id, source_occurrence) =
                semantic_candidate_identity(chunk)?;
            let candidate = CompactCandidate {
                anchor_id: anchor_id.clone(),
                logical_evidence_id,
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(code.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: EvidenceRole::Primary,
                retriever: RetrieverKind::Semantic,
                retriever_revision: ComponentRevision::new("retriever.semantic-flat.evaluation.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                score_domain: ScoreDomainId::new("score.semantic-distance.evaluation.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                raw_score: FixedPointScore::ZERO,
                ordinal_rank: ordinal as u32,
                exact_admission_proof: None,
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-semantic:{}",
                    chunk_id.as_str()
                ))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                freshness: freshness.clone(),
            };
            rows.push(SemanticVectorRecordV1 {
                vector_generation: generation.clone(),
                projection_key: prepared.request.target_projection_key.clone(),
                source_generation: prepared.request.changes.to_generation.clone(),
                chunk_id: chunk_id.clone(),
                candidate,
                binding: CodeCandidateBindingV1 {
                    candidate_anchor: anchor_id,
                    occurrence: CodeOccurrenceRefV1 {
                        generation: chunk.anchor.generation_id.clone(),
                        file: chunk.anchor.file_occurrence_id.clone(),
                        symbol: chunk.anchor.symbol_occurrence_id.clone(),
                        chunk: Some(chunk_id.clone()),
                    },
                    language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                    matched_term_kinds: Vec::new(),
                    source_occurrence,
                },
                values: vector.values.clone(),
            });
        }
        Ok(Self {
            generation,
            projection_key: prepared.request.target_projection_key.clone(),
            search_index_key,
            source_generation: prepared.request.changes.to_generation.clone(),
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            rows,
        })
    }

    fn new(
        vectors: PublishedVectorGenerationV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
    ) -> Result<Self, RetrievalPortError> {
        if vectors.source_generation() != &code.manifest().generation_id {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        let freshness = production_code_index_freshness(
            code.manifest().seal.sealed_at,
            ComponentRevision::new("policy.semantic.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let chunks = code
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (&chunk.id, chunk))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::with_capacity(vectors.vectors().len());
        for (ordinal, (chunk_id, vector)) in vectors.vectors().iter().enumerate() {
            let chunk = chunks
                .get(chunk_id)
                .ok_or(RetrievalPortError::GenerationMismatch)?;
            let (anchor_id, logical_evidence_id, source_occurrence) =
                semantic_candidate_identity(chunk)?;
            let candidate = CompactCandidate {
                anchor_id: anchor_id.clone(),
                logical_evidence_id,
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(code.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: EvidenceRole::Primary,
                retriever: RetrieverKind::Semantic,
                retriever_revision: ComponentRevision::new("retriever.semantic-flat.daemon.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                score_domain: ScoreDomainId::new("score.semantic-distance.daemon.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                raw_score: FixedPointScore::ZERO,
                ordinal_rank: ordinal as u32,
                exact_admission_proof: None,
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-semantic:{}",
                    chunk_id.as_str()
                ))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                freshness: freshness.clone(),
            };
            rows.push(SemanticVectorRecordV1 {
                vector_generation: vectors.generation_id().clone(),
                projection_key: vectors.projection_key().clone(),
                source_generation: vectors.source_generation().clone(),
                chunk_id: chunk_id.clone(),
                candidate,
                binding: CodeCandidateBindingV1 {
                    candidate_anchor: anchor_id,
                    occurrence: CodeOccurrenceRefV1 {
                        generation: chunk.anchor.generation_id.clone(),
                        file: chunk.anchor.file_occurrence_id.clone(),
                        symbol: chunk.anchor.symbol_occurrence_id.clone(),
                        chunk: Some(chunk_id.clone()),
                    },
                    language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                    matched_term_kinds: Vec::new(),
                    source_occurrence,
                },
                values: vector.values.clone(),
            });
        }
        Ok(Self {
            generation: vectors.generation_id().clone(),
            projection_key: vectors.projection_key().clone(),
            search_index_key,
            source_generation: vectors.source_generation().clone(),
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            rows,
        })
    }
}

impl SemanticVectorReadPort for PublishedSemanticVectorReadPortV1 {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        if request.search_kind != SemanticSearchKindV1::ExactFlat
            || request.vector_generation != &self.generation
            || request.projection_key != &self.projection_key
            || request.search_index_key != &self.search_index_key
            || request.source_generation != &self.source_generation
            || request.capability_manifest_digest != &self.capability_manifest_digest
        {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        for row in &self.rows {
            visit(row)?;
        }
        Ok(SemanticVectorScanSummaryV1 {
            examined: self.rows.len() as u64,
            eligible: self.rows.len() as u64,
            excluded: 0,
            unknown: 0,
        })
    }
}

fn semantic_projection_request(
    generation: &CodeIndexPublishedGenerationV1,
    projection: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    current: Option<&SemanticGenerationPointerV1>,
) -> Result<ProjectionBatchRequestV1, SemanticRuntimeScheduleFailureV1> {
    let source = generation.projection().request();
    let incremental = current.is_some_and(|pointer| {
        source.changes.from_generation.as_ref() == Some(&pointer.source_generation)
            && projection.projection_key() == &pointer.projection_key
    });
    let mut changes = if incremental {
        source.changes.clone()
    } else {
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: generation.manifest().generation_id.clone(),
            manifest_digest: source.changes.manifest_digest.clone(),
            added_or_changed: generation
                .chunks()
                .chunks()
                .iter()
                .map(|chunk| ChangedCodeChunkV1 {
                    chunk_id: chunk.id.clone(),
                    prior_digest: None,
                    current_digest: Some(chunk.content_digest.clone()),
                })
                .collect(),
            deleted: Vec::new(),
            reused: Vec::new(),
        };
        changes.manifest_digest = changes
            .compute_digest()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        changes
    };
    // Recompute the manifest digest even for an incremental retarget so a
    // malformed source handoff cannot cross the semantic boundary.
    changes.manifest_digest = changes
        .compute_digest()
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
    let mut request = ProjectionBatchRequestV1 {
        request_digest: changes.manifest_digest.clone(),
        changes,
        previous_projection_key: incremental.then(|| projection.projection_key().clone()),
        target_projection_key: projection.projection_key().clone(),
        replay_reason: if incremental {
            ProjectionReplayReasonV1::SourceEdit
        } else {
            ProjectionReplayReasonV1::FullRebuildIncompatible
        },
    };
    request.request_digest = expected_request_digest(&request)
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
    Ok(request)
}

fn projection_input_bytes(
    chunks: &[CodeSearchChunkV1],
) -> Result<u64, SemanticRuntimeScheduleFailureV1> {
    chunks.iter().try_fold(0_u64, |total, chunk| {
        let bytes = u64::try_from(chunk.sanitized_text.as_str().len())
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        total
            .checked_add(bytes)
            .ok_or(SemanticRuntimeScheduleFailureV1::Projection)
    })
}

fn evaluation_projection_plan(
    generation: &CodeIndexPublishedGenerationV1,
    prepared: &PreparedVectorGenerationV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> Result<VectorGenerationPlanV1, SemanticRuntimeScheduleFailureV1> {
    if prepared.request.changes.to_generation != generation.manifest().generation_id {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    Ok(VectorGenerationPlanV1 {
        target_projection_key: prepared.request.target_projection_key.clone(),
        source_generation: prepared.request.changes.to_generation.clone(),
        source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
        expected_chunk_ids: generation
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect(),
        base_generation,
    })
}

async fn publish_evaluation_projection_case_sqlite(
    store: &DatabaseVectorEvaluationStoreV1<'_>,
    generation: &CodeIndexPublishedGenerationV1,
    prepared: PreparedVectorGenerationV1,
    base_generation: Option<VectorGenerationIdV1>,
    expected_active: Option<&VectorGenerationIdV1>,
) -> Result<
    crate::store::vector_generations::VectorGenerationPublicationV1,
    SemanticRuntimeScheduleFailureV1,
> {
    let plan = evaluation_projection_plan(generation, &prepared, base_generation)?;
    let build = store
        .rebuild_generation(plan)
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
    store
        .commit_batch(&build, None, prepared)
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
    let publication = store
        .publish_generation(&build, expected_active)
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
    if store
        .active_generation_id()
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
        != Some(publication.generation_id.clone())
    {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    Ok(publication)
}

fn projection_case_sample_from_prepared(
    prepared: &PreparedVectorGenerationV1,
    elapsed_micros: u64,
    input_bytes: u64,
    outcome: SemanticProjectionCaseOutcomeV1,
) -> SemanticProjectionCaseSampleV1 {
    let mut chunks_added_or_changed = 0_u64;
    let mut chunks_deleted = 0_u64;
    let mut chunks_reused = 0_u64;
    for receipt in &prepared.receipt.receipts {
        match receipt.operation {
            ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                chunks_added_or_changed += 1;
            }
            ProjectionOperationV1::Deleted => chunks_deleted += 1,
            ProjectionOperationV1::Reused => chunks_reused += 1,
        }
    }
    SemanticProjectionCaseSampleV1 {
        outcome,
        elapsed_micros,
        input_bytes,
        chunks_added_or_changed,
        chunks_deleted,
        chunks_reused,
        projection_calls: prepared.vectors.len() as u64,
    }
}

/// Application search admits semantics only when `query_factory` observes the
/// atomically current compatible generation.
pub fn semantic_lane_readiness_for_request<'a>(
    handle: &DaemonSemanticRuntimeHandleV1,
    request: &'a SemanticRetrievalRequestV1<'a>,
    generation: &'a CompleteSemanticGenerationV1,
    calibration: Option<&'a SemanticCalibrationProfileV1>,
) -> SemanticLaneReadinessV1<'a> {
    match handle.query_factory(
        &request.code_generation,
        &request.vector_generation,
        request.projection.projection_key(),
    ) {
        Some(_) => SemanticLaneReadinessV1::Ready {
            request,
            generation,
            calibration,
        },
        None => SemanticLaneReadinessV1::Unavailable(index_state_from_status(handle.status())),
    }
}

/// Obtain a query factory only for the atomically current generation.
#[cfg(any(test, feature = "semantic-fastembed"))]
pub fn current_query_factory(
    handle: &DaemonSemanticRuntimeHandleV1,
) -> Option<(SemanticGenerationPointerV1, DaemonSemanticQueryFactoryV1)> {
    let pointer = handle.current()?;
    let factory = handle.query_factory(
        &pointer.source_generation,
        &pointer.generation,
        &pointer.projection_key,
    )?;
    Some((pointer, factory))
}

fn execute_calibrated_semantic_query<'a, L>(
    lane: &'a L,
    readiness: SemanticLaneReadinessV1<'a>,
    mode: SemanticQueryModeV1,
    fallback: Arc<QueryFallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    L: SemanticLaneRetriever,
{
    let availability = match &readiness {
        SemanticLaneReadinessV1::Ready { .. } => RetrievalAvailabilityV1::Ready,
        SemanticLaneReadinessV1::Unavailable(state) => match state {
            SemanticIndexStateV1::Unavailable => RetrievalAvailabilityV1::Unavailable,
            SemanticIndexStateV1::Indexing => RetrievalAvailabilityV1::Indexing,
            SemanticIndexStateV1::Degraded => RetrievalAvailabilityV1::Degraded,
            SemanticIndexStateV1::Failed => RetrievalAvailabilityV1::Failed,
            SemanticIndexStateV1::Stale => RetrievalAvailabilityV1::Stale,
            SemanticIndexStateV1::Incompatible => RetrievalAvailabilityV1::Incompatible,
        },
    };
    let requirement = match mode {
        SemanticQueryModeV1::FallbackAllowed => RetrievalRequirementV1::FallbackAllowed,
        SemanticQueryModeV1::StrictSemantic => RetrievalRequirementV1::StrictSemantic,
    };
    let on_abstention = match mode {
        SemanticQueryModeV1::FallbackAllowed => SemanticAbstentionDispositionV1::UseFallback,
        SemanticQueryModeV1::StrictSemantic => SemanticAbstentionDispositionV1::RejectUnavailable,
    };
    let decision = match select_retrieval(availability, requirement) {
        RetrievalSelectionV1::Semantic => {
            SemanticQueryDecisionV1::ExecuteSemantic { on_abstention }
        }
        RetrievalSelectionV1::FrozenFallback => SemanticQueryDecisionV1::UseFallback,
        RetrievalSelectionV1::Unavailable => SemanticQueryDecisionV1::RejectUnavailable,
    };
    CalibratedSemanticQueryService::new(lane).execute(readiness, decision, fallback)
}

/// Complete input set for one application semantic-search composition.
pub struct ApplicationSemanticSearchParametersV1<'a, V, C> {
    pub handle: &'a DaemonSemanticRuntimeHandleV1,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
    pub generation: &'a CompleteSemanticGenerationV1,
    pub calibration: Option<&'a SemanticCalibrationProfileV1>,
    pub vectors: &'a V,
    pub control: &'a C,
    pub mode: SemanticQueryModeV1,
    pub fallback: Arc<QueryFallbackSubpayload>,
}

/// Application search composition: admit `SemanticCodeRetriever` only through
/// [`DaemonSemanticRuntimeHandleV1::query_factory`].
///
/// Non-ready / indexing / degraded states never construct the retriever and
/// return the frozen query fallback without waiting on `FastEmbed` download or
/// projection. Exact/lexical/graph owners stay independently callable.
pub fn compose_application_semantic_search<'a, V, C>(
    parameters: ApplicationSemanticSearchParametersV1<'a, V, C>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    V: SemanticVectorReadPort,
    C: SemanticExecutionControl + Sync,
{
    let ApplicationSemanticSearchParametersV1 {
        handle,
        request,
        generation,
        calibration,
        vectors,
        control,
        mode,
        fallback,
    } = parameters;
    let readiness = semantic_lane_readiness_for_request(handle, request, generation, calibration);
    match readiness {
        SemanticLaneReadinessV1::Ready {
            request,
            generation,
            calibration,
        } => {
            let Some(factory) = handle.query_factory(
                &request.code_generation,
                &request.vector_generation,
                request.projection.projection_key(),
            ) else {
                // Atomically current generation is the only admission path.
                return execute_calibrated_semantic_query(
                    &NeverCalledSemanticLane,
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Incompatible),
                    mode,
                    fallback,
                );
            };
            let embedder = factory.create(control);
            let lane = SemanticCodeRetriever::new(&embedder, vectors, control);
            execute_calibrated_semantic_query(
                &lane,
                SemanticLaneReadinessV1::Ready {
                    request,
                    generation,
                    calibration,
                },
                mode,
                fallback,
            )
        }
        unavailable @ SemanticLaneReadinessV1::Unavailable(_) => {
            execute_calibrated_semantic_query(&NeverCalledSemanticLane, unavailable, mode, fallback)
        }
    }
}

/// Project-scoped application search consumer over the retained production
/// runtime and durable active vector generation.
pub async fn compose_project_application_semantic_search<C>(
    project_root: &Path,
    code_generation: &CodeIndexPublishedGenerationV1,
    request: &SemanticRetrievalRequestV1<'_>,
    calibration: Option<&SemanticCalibrationProfileV1>,
    control: &C,
    mode: SemanticQueryModeV1,
    fallback: Arc<QueryFallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    C: SemanticExecutionControl + Sync,
{
    let Some(runtime) = project_semantic_production_runtime(project_root) else {
        return execute_calibrated_semantic_query(
            &NeverCalledSemanticLane,
            SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
            mode,
            fallback,
        );
    };
    runtime
        .execute_search(
            code_generation,
            request,
            calibration,
            control,
            mode,
            fallback,
        )
        .await
}

/// Production execution bridge for callers that already own authenticated
/// semantic request material and an authenticated frozen query composition.
///
/// MCP cannot use this bridge until its query-MAC and query composition
/// authorities are mounted; accepting the typed request here prevents that
/// boundary from inventing either input.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProductionProjectSemanticSearchBridgeV1;

/// Authenticated project-scoped inputs for production semantic search.
pub struct AuthorizedProjectSemanticSearchParametersV1<'a, C> {
    pub project_root: &'a Path,
    pub code_generation: &'a CodeIndexPublishedGenerationV1,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
    pub calibration: Option<&'a SemanticCalibrationProfileV1>,
    pub control: &'a C,
    pub mode: SemanticQueryModeV1,
    pub authorized_query: &'a AuthorizedQueryFallbackV1,
}

impl ProductionProjectSemanticSearchBridgeV1 {
    pub fn execute<'a, C>(
        &'a self,
        parameters: AuthorizedProjectSemanticSearchParametersV1<'a, C>,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>>
    where
        C: SemanticExecutionControl + Sync + 'a,
    {
        let AuthorizedProjectSemanticSearchParametersV1 {
            project_root,
            code_generation,
            request,
            calibration,
            control,
            mode,
            authorized_query,
        } = parameters;
        if request.query_digest != authorized_query.query_digest {
            return Box::pin(async { Err(SemanticQueryServiceError::InvalidFallback) });
        }
        Box::pin(compose_project_application_semantic_search(
            project_root,
            code_generation,
            request,
            calibration,
            control,
            mode,
            Arc::clone(&authorized_query.fallback),
        ))
    }
}

struct NeverCalledSemanticLane;

impl SemanticLaneRetriever for NeverCalledSemanticLane {
    fn retrieve_semantic(
        &self,
        _request: &SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        Err(RetrievalPortError::Contract(
            "non-ready semantic lane must never be invoked".to_owned(),
        ))
    }
}

/// Daemon backend that surfaces schedule projection through the application port.
pub struct DaemonSemanticRuntimeBackendV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    production: Option<ProductionSemanticRuntimeV1>,
    configuration: Mutex<Option<SemanticConfigurationPinV1>>,
}

impl DaemonSemanticRuntimeBackendV1 {
    #[cfg(test)]
    pub fn new(handle: DaemonSemanticRuntimeHandleV1) -> Self {
        Self {
            handle,
            production: None,
            configuration: Mutex::new(None),
        }
    }

    pub fn from_production(runtime: ProductionSemanticRuntimeV1) -> Self {
        Self {
            handle: runtime.handle.clone(),
            production: Some(runtime),
            configuration: Mutex::new(None),
        }
    }

    pub fn bind_configuration(&self, pin: SemanticConfigurationPinV1) {
        *self
            .configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pin);
    }

    pub fn application_status(&self) -> SemanticRuntimeStatusV1 {
        let configuration = self
            .configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        application_status_from_projection(&self.handle.status_projection(), configuration)
    }
}

impl SemanticRuntimeBackendV1 for DaemonSemanticRuntimeBackendV1 {
    fn status<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRuntimeStateV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(configuration.clone());
            Ok(self.application_status().state)
        })
    }

    fn activate<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticActivationReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(command.configuration.clone());
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        })
    }

    fn rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(command.configuration.clone());
            let runtime = self
                .production
                .as_ref()
                .ok_or(SemanticRuntimeBackendErrorV1::Unavailable)?;
            if let Some(target_generation) = command.request.target_generation.as_ref() {
                runtime
                    .rollback(
                        target_generation,
                        &command.request.expected_active_generation,
                    )
                    .await
                    .map_err(|error| match error {
                        SemanticRuntimeScheduleFailureV1::Artifact
                        | SemanticRuntimeScheduleFailureV1::Runtime => {
                            SemanticRuntimeBackendErrorV1::Unavailable
                        }
                        SemanticRuntimeScheduleFailureV1::Projection
                        | SemanticRuntimeScheduleFailureV1::Publication
                        | SemanticRuntimeScheduleFailureV1::Cancelled => {
                            SemanticRuntimeBackendErrorV1::Conflict
                        }
                    })?;
            }
            SemanticRollbackReceiptV1::issue(command, now_micros())
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
        })
    }
}

fn index_state_from_status(status: SemanticRuntimeScheduleStatusV1) -> SemanticIndexStateV1 {
    match status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticIndexStateV1::Unavailable,
        SemanticRuntimeScheduleStatusV1::Indexing { .. } => SemanticIndexStateV1::Indexing,
        SemanticRuntimeScheduleStatusV1::Failed { .. } => SemanticIndexStateV1::Failed,
        SemanticRuntimeScheduleStatusV1::Current { .. } => SemanticIndexStateV1::Incompatible,
    }
}

fn provisional_vector_generation(source: &CodeGenerationId) -> VectorGenerationIdV1 {
    let digest = canonical_sha256(&("semantic.indexing.target", source)).unwrap_or_else(|_| {
        ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
            .unwrap_or_else(|_| panic!("digest"))
    });
    VectorGenerationIdV1::new(digest)
}

/// Process-local registry so Doctor/`tracedecay_runtime` can observe the
/// daemon-private scheduler without a wire operation.
fn project_semantic_handles() -> &'static Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>> {
    static HANDLES: OnceLock<Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>>> =
        OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn project_semantic_production_runtimes()
-> &'static Mutex<BTreeMap<PathBuf, ProductionSemanticRuntimeV1>> {
    static RUNTIMES: OnceLock<Mutex<BTreeMap<PathBuf, ProductionSemanticRuntimeV1>>> =
        OnceLock::new();
    RUNTIMES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Retain a project semantic handle for status/search composition.
pub fn register_project_semantic_runtime(
    project_root: PathBuf,
    handle: DaemonSemanticRuntimeHandleV1,
) {
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root, handle);
}

/// Drop a retained project semantic handle.
pub fn unregister_project_semantic_runtime(project_root: &Path) {
    super::unregister_project_semantic_redundancy_generation(project_root);
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
}

pub fn project_semantic_production_runtime(
    project_root: &Path,
) -> Option<ProductionSemanticRuntimeV1> {
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()
}

/// Source generation bound to the atomically current semantic pointer.
///
/// Query adapters compare this identity with the exact sealed code generation
/// selected at admission. A stale or merely indexing vector generation never
/// becomes eligible for semantic composition.
pub fn project_semantic_source_generation(project_root: &Path) -> Option<CodeGenerationId> {
    project_semantic_generation_pointer(project_root).map(|pointer| pointer.source_generation)
}

pub(crate) fn project_semantic_generation_pointer(
    project_root: &Path,
) -> Option<SemanticGenerationPointerV1> {
    if let Some(runtime) = project_semantic_production_runtime(project_root) {
        return runtime.handle.current();
    }
    project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .and_then(DaemonSemanticRuntimeHandleV1::current)
}

/// Application status for a mounted project semantic scheduler, if any.
pub fn project_semantic_application_status(
    project_root: &Path,
    configuration: Option<SemanticConfigurationPinV1>,
) -> Option<SemanticRuntimeStatusV1> {
    if let Some(runtime) = project_semantic_production_runtime(project_root) {
        let backend = DaemonSemanticRuntimeBackendV1::from_production(runtime);
        if let Some(configuration) = configuration {
            backend.bind_configuration(configuration);
        }
        return Some(backend.application_status());
    }
    let handle = project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()?;
    Some(application_status_from_projection(
        &handle.status_projection(),
        configuration,
    ))
}

/// Hook invoked after a code generation publishes; must not block search.
pub type SavedCodeGenerationScheduleHookV1 =
    Arc<dyn Fn(&CodeIndexPublishedGenerationV1) -> bool + Send + Sync>;

/// Owned authorities and identities captured by a saved-generation hook.
pub struct SavedGenerationScheduleHookParametersV1 {
    pub project_root: PathBuf,
    pub code_index_store_root: PathBuf,
    pub worktree_id: WorktreeId,
    pub handle: DaemonSemanticRuntimeHandleV1,
    pub database: Arc<Database>,
    pub lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    pub resources: SemanticResourceCeilings,
    pub fair_scheduler: DaemonGlobalSemanticProjectionSchedulerV1,
}

/// Production hook: enqueue semantic projection for each saved generation.
///
/// Artifact admission remains owned by the model lifecycle. Until a complete
/// compatible artifact is available the background task fails closed without
/// joining into exact/lexical/graph search.
pub fn production_saved_generation_schedule_hook(
    parameters: SavedGenerationScheduleHookParametersV1,
) -> SavedCodeGenerationScheduleHookV1 {
    let SavedGenerationScheduleHookParametersV1 {
        project_root,
        code_index_store_root,
        worktree_id,
        handle,
        database,
        lifecycle,
        resources,
        fair_scheduler,
    } = parameters;
    let runtime = Arc::new(ProductionSemanticRuntimeV1::new_with_code_index_store_root(
        handle,
        database,
        code_index_store_root,
        lifecycle,
        resources,
    ));
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root.clone(), runtime.as_ref().clone());
    Arc::new(move |generation| {
        if generation.snapshot().worktree.as_ref() != Some(&worktree_id) {
            return false;
        }
        super::register_project_semantic_redundancy_generation(
            project_root.clone(),
            generation.clone(),
        );
        let runtime = Arc::clone(&runtime);
        let generation = generation.clone();
        let Ok(tokio) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        let queued_bytes = generation
            .chunks()
            .chunks()
            .iter()
            .fold(0_u64, |total, chunk| {
                total.saturating_add(
                    u64::try_from(chunk.sanitized_text.as_str().len()).unwrap_or(u64::MAX),
                )
            });
        let batch = SemanticProjectionBatchV1::new(
            worktree_id.clone(),
            generation.manifest().generation_id.clone(),
            queued_bytes,
            resources.max_resident_bytes,
        );
        fair_scheduler
            .enqueue_work(
                batch,
                Box::new(move |lease| {
                    tokio.spawn(async move {
                        let lease = Arc::new(lease);
                        if lease.is_cancelled() {
                            return;
                        }
                        let cancellation_lease = Arc::clone(&lease);
                        let cancelled: Arc<dyn Fn() -> bool + Send + Sync> =
                            Arc::new(move || cancellation_lease.is_cancelled());
                        if runtime
                            .migrate_legacy_vectors_for_generation(&generation, cancelled)
                            .await
                            .is_err()
                            || lease.is_cancelled()
                        {
                            return;
                        }
                        let Ok(lease) = Arc::try_unwrap(lease) else {
                            return;
                        };
                        match runtime.restore_current(&generation).await {
                            Ok(true) => {}
                            Ok(false) | Err(_) => {
                                let _ = runtime.schedule_saved_generation_fair(&generation, lease);
                            }
                        }
                    });
                }),
            )
            .is_ok()
    })
}

fn fair_schedule_failure(
    error: SemanticProjectionScheduleErrorV1,
) -> SemanticRuntimeScheduleFailureV1 {
    match error {
        SemanticProjectionScheduleErrorV1::Cancelled => SemanticRuntimeScheduleFailureV1::Cancelled,
        SemanticProjectionScheduleErrorV1::QueueBytesCapacity { .. }
        | SemanticProjectionScheduleErrorV1::QueueBatchCapacity { .. }
        | SemanticProjectionScheduleErrorV1::SessionMemoryReservationTooLarge { .. }
        | SemanticProjectionScheduleErrorV1::PublicationCapacity { .. }
        | SemanticProjectionScheduleErrorV1::PublicationAlreadyClaimed => {
            SemanticRuntimeScheduleFailureV1::Publication
        }
    }
}

fn retained_readable_sources(
    inventory: &tracedecay_semantic::legacy_migration::LegacyVectorInventoryV1,
) -> BTreeSet<CodeGenerationId> {
    inventory.retained_readable_sources()
}

fn load_retained_code_generations(
    generations_root: &Path,
    current: &CodeIndexPublishedGenerationV1,
    inventory: &tracedecay_semantic::legacy_migration::LegacyVectorInventoryV1,
) -> Result<
    BTreeMap<CodeGenerationId, CodeIndexPublishedGenerationV1>,
    SemanticRuntimeScheduleFailureV1,
> {
    let required = retained_readable_sources(inventory);
    let mut retained = BTreeMap::new();
    if required.contains(&current.manifest().generation_id) {
        retained.insert(current.manifest().generation_id.clone(), current.clone());
    }
    if retained.len() == required.len() {
        return Ok(retained);
    }

    let entries = match std::fs::read_dir(generations_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(retained),
        Err(_) => return Err(SemanticRuntimeScheduleFailureV1::Publication),
    };
    let mut paths = entries
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with("generation-") || !file_name.ends_with(".json") {
            continue;
        }
        let bytes =
            std::fs::read(&path).map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        if !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(&bytes)
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?
        {
            continue;
        }
        let generation = CodeIndexPublishedGenerationV1::decode_sealed(&bytes)
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        let source = generation.manifest().generation_id.clone();
        if required.contains(&source) {
            retained.entry(source).or_insert(generation);
            if retained.len() == required.len() {
                break;
            }
        }
    }
    Ok(retained)
}

fn retained_canonical_chunk_sets<Load>(
    inventory: &tracedecay_semantic::legacy_migration::LegacyVectorInventoryV1,
    mut load: Load,
) -> Result<Vec<CanonicalEligibleChunkSetV1>, SemanticRuntimeScheduleFailureV1>
where
    Load: FnMut(
        &CodeGenerationId,
    ) -> Result<Option<Vec<CodeSearchChunkV1>>, SemanticRuntimeScheduleFailureV1>,
{
    retained_readable_sources(inventory)
        .into_iter()
        .filter_map(|source| match load(&source) {
            Ok(Some(chunks)) => Some(Ok((source, chunks))),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .map(|result| {
            let (source, chunks) = result?;
            CanonicalEligibleChunkSetV1::try_from_chunks(source, chunks)
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)
        })
        .collect()
}

fn map_legacy_store(
    error: crate::store::vector_generations::VectorGenerationStoreErrorV1,
) -> LegacyVectorMigrationErrorV1 {
    LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string())
}

async fn replace_legacy_vectors_after_rebuild(
    store: &DatabaseVectorGenerationStoreV1<'_>,
    inventory: &DatabaseLegacyVectorInventoryV1,
    replacement: FakeVectorGenerationStoreV1,
    transaction: &LegacyVectorMigrationOwnerTransactionV1,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> Result<LegacyVectorMigrationReceiptV1, SemanticRuntimeScheduleFailureV1> {
    if cancelled() {
        return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
    }
    store
        .replace_legacy_vectors_atomically(inventory, replacement, transaction)
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    use tracedecay_domain::UtcMicros;

    use tokio::sync::oneshot;
    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
    use tracedecay_domain::{
        AdmittedEmbeddingProjectionKeyV1, AuthorizationRevision, BoundedSanitizedText,
        CalibrationProfileId, ChangedCodeChunkSetV1, ChunkerRevision, CodeGenerationId,
        CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1,
        ContentDigest, EphemeralSanitizedQueryViewV1, FallbackSubpayloadDigest, FileOccurrenceId,
        FusionProfileId, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId, PrincipalId,
        ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionReplayReasonV1, PublicRetrieverStatus,
        QueryDigest, QueryMac, QueryNormalizationRevision, RepositoryId, RetrievalRequest,
        RetrievalScope, RetrievalSnapshot, RetrieverKind, SanitizerRevision, SensitivityDecision,
        SensitivityLevelV1, SingleRootScopeV1, SourceSpan, TemporalModeV1, VectorGenerationIdV1,
        VectorWatermark,
    };

    use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
    use tracedecay_semantic::legacy_migration::ProductionLegacyVectorCanonicalRebuilderV1;
    use tracedecay_semantic::{
        DaemonSemanticRuntimeHandleV1, FastEmbedSemanticGenerationRequestV1,
        PreparedSemanticRuntimeCommitV1, SemanticGenerationPointerV1,
        SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1, SemanticRuntimeWorkV1,
    };

    use super::*;

    fn source_generation(value: char) -> CodeGenerationId {
        CodeGenerationId::new(format!("code-generation.{value}")).expect("source generation")
    }

    fn vector_generation(value: char) -> VectorGenerationIdV1 {
        VectorGenerationIdV1::new(
            canonical_sha256(&("semantic.test.vector-generation", value)).expect("manifest digest"),
        )
    }

    fn projection_key() -> ProjectionKeyV1 {
        // Derive the projection key from the same admitted authority the query
        // runtime binds against so `profile_digest` is the canonical digest the
        // embedding projection produces, rather than a non-canonical placeholder.
        tracedecay_semantic::session_pool::test_support::authority()
            .projection()
            .projection_key()
            .clone()
    }

    fn search_index_key() -> &'static SemanticSearchIndexKeyV1 {
        static KEY: std::sync::OnceLock<SemanticSearchIndexKeyV1> = std::sync::OnceLock::new();
        KEY.get_or_init(|| {
            SemanticSearchIndexProfileV1::exact_flat_v1()
                .and_then(|profile| profile.index_key())
                .expect("exact-flat search index key")
        })
    }

    fn pointer(vector: char, source: char) -> SemanticGenerationPointerV1 {
        SemanticGenerationPointerV1 {
            generation: vector_generation(vector),
            source_generation: source_generation(source),
            projection_key: projection_key(),
        }
    }

    fn configuration_pin() -> SemanticConfigurationPinV1 {
        SemanticConfigurationPinV1 {
            revision_id: ConfigurationRevisionId::try_from(
                "configuration.revision.semantic-test".to_owned(),
            )
            .expect("configuration revision"),
            snapshot_id: ConfigurationSnapshotId::try_from(
                "configuration.snapshot.semantic-test".to_owned(),
            )
            .expect("configuration snapshot"),
            effective_behavior_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                .expect("configuration digest"),
        }
    }

    fn composition_request<'a>(
        query_view: &'a EphemeralSanitizedQueryViewV1,
        projection: &'a AdmittedEmbeddingProjectionKeyV1,
        source: CodeGenerationId,
        vector: VectorGenerationIdV1,
    ) -> SemanticRetrievalRequestV1<'a> {
        let query_digest = QueryDigest::new(
            projection.privacy_domain().clone(),
            projection.privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "33".repeat(32))).expect("query MAC"),
        );
        let budget = tracedecay_domain::RetrievalBudget {
            max_candidates_per_lane: 8,
            max_fused_candidates: 16,
            max_hydrated_results: 8,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        };
        SemanticRetrievalRequestV1 {
            base: RetrievalRequest {
                principal: PrincipalId::try_from("principal.fixture".to_owned())
                    .expect("principal"),
                scope: RetrievalScope {
                    privacy_domain: projection.privacy_domain().clone(),
                    root: SingleRootScopeV1 {
                        repository: RepositoryId::try_from("repository.fixture".to_owned())
                            .expect("repository"),
                        worktree: None,
                        reference: None,
                    },
                },
                temporal_mode: TemporalModeV1::Current,
                snapshot: RetrievalSnapshot {
                    watermarks: VectorWatermark::default(),
                    freshness_digest: tracedecay_domain::FreshnessVectorDigest::try_from(format!(
                        "sha256:{}",
                        "a".repeat(64)
                    ))
                    .expect("freshness"),
                    authorization_revision: AuthorizationRevision::try_from(
                        "authorization.v1".to_owned(),
                    )
                    .expect("authorization"),
                    captured_at: UtcMicros(1),
                },
                profile_id: FusionProfileId::try_from("profile.semantic.v1".to_owned())
                    .expect("profile"),
                budget,
            },
            query_digest,
            query_view,
            projection,
            search_index_key: search_index_key(),
            capability_manifest_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("capability"),
            vector_generation: vector,
            code_generation: source,
            budget,
        }
    }

    fn composition_fallback() -> Arc<QueryFallbackSubpayload> {
        let mut fallback = QueryFallbackSubpayload {
            profile_id: FusionProfileId::try_from("profile.query.semantic-contract.v1".to_owned())
                .expect("profile"),
            ordered_candidates: Vec::new(),
            public_fallback_lane_coverage: BTreeMap::from([
                (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
                (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
                (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
            ]),
            freshness: Vec::new(),
            cursor: None,
            digest: FallbackSubpayloadDigest::new(format!("sha256:{}", "0".repeat(64)))
                .unwrap_or_else(|_| panic!("digest")),
        };
        fallback.digest = fallback.compute_digest().expect("fallback digest");
        Arc::new(fallback)
    }

    fn composition_calibration(
        request: &SemanticRetrievalRequestV1<'_>,
    ) -> SemanticCalibrationProfileV1 {
        SemanticCalibrationProfileV1 {
            calibration_profile_id: CalibrationProfileId::try_from(
                "calibration.semantic.fixture.v1".to_owned(),
            )
            .expect("calibration profile"),
            cohort_digest: ManifestDigest::new(format!("sha256:{}", "7".repeat(64)))
                .expect("cohort digest"),
            projection_key: request.projection.projection_key().clone(),
            vector_generation: request.vector_generation.clone(),
            capability_manifest_digest: request.capability_manifest_digest.clone(),
            maximum_distance_micros: i64::MAX,
            minimum_margin_micros: 0,
        }
    }

    fn projection_request(source: char) -> ProjectionBatchRequestV1 {
        ProjectionBatchRequestV1 {
            request_digest: ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
                .expect("request digest"),
            changes: ChangedCodeChunkSetV1 {
                from_generation: None,
                to_generation: source_generation(source),
                manifest_digest: ManifestDigest::new(format!("sha256:{}", "d".repeat(64)))
                    .expect("source manifest"),
                added_or_changed: Vec::new(),
                deleted: Vec::new(),
                reused: Vec::new(),
            },
            previous_projection_key: None,
            target_projection_key: projection_key(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        }
    }

    fn canonical_chunk(source: &CodeGenerationId, value: char) -> CodeSearchChunkV1 {
        CodeSearchChunkV1 {
            id: CodeSearchChunkId::new(format!("chunk.v1.{value}")).expect("chunk id"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: source.clone(),
                file_occurrence_id: FileOccurrenceId::new(format!("{value}.rs"))
                    .expect("file occurrence"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 4,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: ContentDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
                .expect("content digest"),
            language_descriptor_revision: LanguageDescriptorRevision::new("rust.v1")
                .expect("language descriptor"),
            chunker_revision: ChunkerRevision::new("chunker.v1").expect("chunker revision"),
            sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("sanitizer revision"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.v1").expect("policy revision"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new("code").expect("sanitized text"),
        }
    }

    #[test]
    fn symbol_backed_semantic_identity_dedupes_with_lexical_evidence() {
        let source = source_generation('a');
        let mut chunk = canonical_chunk(&source, 'a');
        let symbol = tracedecay_domain::SymbolOccurrenceId::new("symbol.fixture")
            .expect("symbol occurrence");
        chunk.anchor.symbol_occurrence_id = Some(symbol.clone());

        let (semantic_anchor, semantic_logical_evidence, source_occurrence) =
            semantic_candidate_identity(&chunk).expect("semantic identity");
        let lexical_evidence = format!("code-symbol:{}", symbol.as_str());
        let lexical_anchor =
            RetrievalAnchorId::new(lexical_evidence.clone()).expect("lexical anchor");
        let mixed_anchors = BTreeSet::from([semantic_anchor.clone(), lexical_anchor]);

        assert_eq!(mixed_anchors.len(), 1);
        assert_eq!(
            semantic_anchor,
            RetrievalAnchorId::new(lexical_evidence.clone()).expect("semantic anchor")
        );
        assert_eq!(
            semantic_logical_evidence,
            LogicalEvidenceId::new(lexical_evidence).expect("logical evidence")
        );
        assert_eq!(
            source_occurrence,
            SourceOccurrenceId::new(format!("code-chunk:{}", chunk.id.as_str()))
                .expect("source occurrence")
        );
    }

    #[derive(Clone)]
    struct LegacyInventoryFixture(tracedecay_semantic::legacy_migration::LegacyVectorInventoryV1);

    impl LegacyVectorInventoryPortV1 for LegacyInventoryFixture {
        fn read_only_inventory(
            &self,
        ) -> Result<
            tracedecay_semantic::legacy_migration::LegacyVectorInventoryV1,
            LegacyVectorMigrationErrorV1,
        > {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn retained_multi_generation_projections_are_all_rebuilt() {
        use tracedecay_semantic::legacy_migration::{
            LegacyVectorInventoryEntryV1, LegacyVectorInventoryV1,
        };

        let source_a = source_generation('a');
        let source_b = source_generation('b');
        let legacy_a = vector_generation('a');
        let legacy_b = vector_generation('b');
        let inventory = LegacyInventoryFixture(LegacyVectorInventoryV1 {
            expected_active_generation: Some(legacy_b.clone()),
            entries: vec![
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy_a,
                    source_generation: source_a.clone(),
                },
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy_b,
                    source_generation: source_b.clone(),
                },
            ],
        });
        let available = BTreeMap::from([
            (source_a.clone(), vec![canonical_chunk(&source_a, 'a')]),
            (source_b.clone(), vec![canonical_chunk(&source_b, 'b')]),
        ]);
        let retained = retained_canonical_chunk_sets(&inventory.0, |source| {
            Ok(available.get(source).cloned())
        })
        .expect("retained canonical chunks");
        let mut rebuilder =
            ProductionLegacyVectorCanonicalRebuilderV1::try_new(retained, |chunks| {
                let value = if chunks.source_generation() == &source_a {
                    'c'
                } else {
                    'd'
                };
                Ok(StagedCanonicalVectorRebuildV1 {
                    source_generation: chunks.source_generation().clone(),
                    rebuilt_generation: vector_generation(value),
                    canonical_chunk_set_digest: chunks.digest().clone(),
                })
            })
            .expect("rebuilder");

        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("migration");

        assert_eq!(transaction.receipt.counts.rebuilt, 2);
        assert_eq!(transaction.receipt.counts.dropped, 0);
        assert_eq!(rebuilder.staged_rebuilds().len(), 2);
    }

    #[tokio::test]
    async fn cancellation_after_rebuild_cannot_publish_legacy_replacement() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        let authority = DatabaseAuthority::acquire_test(&path, "cancelled legacy replacement")
            .expect("database authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("migration store");
        let inventory = store
            .read_legacy_inventory()
            .await
            .expect("legacy inventory");
        let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(Vec::new(), |_| {
            unreachable!("empty inventory cannot request a rebuild")
        })
        .expect("empty rebuilder");
        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("prepared replacement");
        let before = database
            .query_scalar_i64(
                "read vector revision before cancellation",
                "SELECT revision
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
            )
            .await
            .expect("vector revision");

        assert_eq!(
            replace_legacy_vectors_after_rebuild(
                &store,
                &inventory,
                FakeVectorGenerationStoreV1::new(),
                &transaction,
                &|| true,
            )
            .await,
            Err(SemanticRuntimeScheduleFailureV1::Cancelled)
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove cancelled replacement did not publish",
                    "SELECT revision
                     FROM semantic_vector_generation_state_v1
                     WHERE singleton = 1",
                )
                .await
                .expect("vector revision"),
            before
        );
    }

    #[test]
    fn compatible_generation_uses_projection_change_manifest_digest() {
        let request = projection_request('m');

        assert_eq!(
            semantic_source_manifest_digest(&request),
            &request.changes.manifest_digest
        );
        assert_ne!(
            semantic_source_manifest_digest(&request),
            &request.request_digest,
            "the projection request receipt is not the source manifest identity"
        );
    }

    #[tokio::test]
    async fn saved_edit_schedules_fastembed_without_blocking_exact_search() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let exact_ready = AtomicBool::new(false);

        let request = FastEmbedSemanticGenerationRequestV1::new(
            source_generation('a'),
            projection_request('a'),
            Vec::<CodeSearchChunkV1>::new(),
            move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
            move |_| async move { Err(SemanticRuntimeScheduleFailureV1::Publication) },
        )
        .expect("saved generation request");
        assert!(handle.schedule_generation(request));
        started_rx.await.expect("background schedule started");

        // Ordinary exact search proceeds while FastEmbed work is parked.
        exact_ready.store(true, Ordering::SeqCst);
        assert!(exact_ready.load(Ordering::SeqCst));
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Indexing { .. }
        ));
        release_tx.send(()).expect("release artifact loader");
    }

    #[tokio::test]
    async fn runtime_reports_semantic_indexing_progress() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            4,
            move |progress| async move {
                progress.set_completed_units(2);
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));
        started_rx.await.expect("indexing started");
        let projection = handle.status_projection();
        let status = application_status_from_projection(&projection, None);
        match status.state {
            SemanticRuntimeStateV1::Indexing {
                completed_units,
                total_units,
                ..
            } => {
                assert_eq!(completed_units, 2);
                assert_eq!(total_units, 4);
            }
            other => panic!("expected indexing status, got {other:?}"),
        }
        let _ = release_tx.send(());
    }

    #[tokio::test]
    async fn runtime_reports_degraded_reason_and_prior_generation() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let prior_pointer = pointer('a', 'a');
        let prior = prior_pointer.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(prior_pointer)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("prior generation published");

        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('b'),
            1,
            move |_progress| async move { Err(SemanticRuntimeScheduleFailureV1::Artifact) },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if matches!(
                    handle.status(),
                    SemanticRuntimeScheduleStatusV1::Failed { .. }
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failure observed");

        let projection = handle.status_projection();
        assert_eq!(
            projection.degraded_reason,
            Some(SemanticFallbackReasonV1::ArtifactUnavailable)
        );
        assert_eq!(projection.prior_generation.as_ref(), Some(&prior));
        let status = application_status_from_projection(&projection, None);
        match status.state {
            SemanticRuntimeStateV1::Degraded {
                active_generation,
                reason,
            } => {
                assert_eq!(active_generation.as_ref(), Some(&prior));
                assert_eq!(reason, SemanticFallbackReasonV1::ArtifactUnavailable);
            }
            other => panic!("expected degraded status, got {other:?}"),
        }
        // Prior generation remains queryable / current for compatible reads.
        assert_eq!(
            handle.current().map(|pointer| pointer.generation),
            Some(prior)
        );
    }

    #[tokio::test]
    async fn atomically_current_generation_enables_semantic_lane() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('c', 'c');
        let source = published.source_generation.clone();
        let vector = published.generation.clone();
        let projection_key = published.projection_key.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('c'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        // Pointer alone is insufficient — application search needs a bound query runtime.
        assert!(
            handle
                .query_factory(&source, &vector, &projection_key)
                .is_none(),
            "query_factory must stay closed until the query runtime is bound"
        );
        handle
            .bind_query_runtime_for_current(std::sync::Arc::new(
                tracedecay_semantic::session_pool::test_support::authority(),
            ))
            .expect("bind query runtime for current generation");

        assert!(
            handle
                .query_factory(&source, &vector, &projection_key)
                .is_some(),
            "atomically current generation must enable query_factory"
        );
        assert!(
            current_query_factory(&handle).is_some(),
            "current_query_factory must surface the atomically current factory"
        );
        assert!(
            handle
                .query_factory(&source_generation('x'), &vector, &projection_key)
                .is_none(),
            "incompatible source must not enable semantics"
        );

        let backend = DaemonSemanticRuntimeBackendV1::new(handle.clone());
        let status = backend.application_status();
        assert!(matches!(
            status.route(),
            crate::semantic_runtime::SemanticRuntimeRouteV1::LexicalFallback { .. }
        ));
    }

    #[tokio::test]
    async fn live_request_cancellation_reaches_query_runtime_before_vector_scan() {
        struct PanicVectors;

        impl SemanticVectorReadPort for PanicVectors {
            fn scan_exact_flat(
                &self,
                _request: SemanticVectorReadRequestV1<'_>,
                _visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
            ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
                panic!("cancelled query runtime must not scan vectors")
            }
        }

        struct CancelAtRuntimeBoundary {
            checks: AtomicUsize,
        }

        impl SemanticExecutionControl for CancelAtRuntimeBoundary {
            fn is_cancelled(&self) -> bool {
                self.checks.fetch_add(1, Ordering::SeqCst) != 0
            }

            fn elapsed_micros(&self) -> u64 {
                0
            }
        }

        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('q', 'q');
        let source = published.source_generation.clone();
        let vector = published.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source.clone(),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");
        let authority = tracedecay_semantic::session_pool::test_support::authority();
        handle
            .bind_query_runtime_for_current(Arc::new(authority.clone()))
            .expect("bind query runtime");

        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "cancel before session acquisition",
            SanitizerRevision::try_from("sanitizer.v1".to_owned()).expect("sanitizer"),
            QueryNormalizationRevision::try_from("normalizer.v1".to_owned()).expect("normalizer"),
        )
        .expect("query view");
        let request = composition_request(&query_view, authority.projection(), source, vector);
        let complete = CompleteSemanticGenerationV1::new(
            request.projection.projection_key().clone(),
            request.search_index_key.clone(),
            request.vector_generation.clone(),
            request.code_generation.clone(),
            request.capability_manifest_digest.clone(),
        )
        .expect("complete generation");
        let calibration = composition_calibration(&request);
        let control = CancelAtRuntimeBoundary {
            checks: AtomicUsize::new(0),
        };

        let outcome = compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
            handle: &handle,
            request: &request,
            generation: &complete,
            calibration: Some(&calibration),
            vectors: &PanicVectors,
            control: &control,
            mode: SemanticQueryModeV1::FallbackAllowed,
            fallback: composition_fallback(),
        })
        .expect("cancelled semantic composition");

        assert!(matches!(
            outcome,
            SemanticQueryServiceOutcomeV1::Fallback {
                abstention: tracedecay_query::retrieval::semantic::SemanticAbstentionV1::Cancelled,
                ..
            }
        ));
        assert!(
            control.checks.load(Ordering::SeqCst) >= 2,
            "query runtime must poll the same live request control"
        );
    }

    #[tokio::test]
    async fn current_scheduler_pointer_without_persisted_receipt_stays_degraded() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('r', 'r');
        let generation = published.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('r'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        let status = application_status_from_projection(
            &handle.status_projection(),
            Some(configuration_pin()),
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        let restarted_status = application_status_from_projection(
            &handle.status_projection(),
            Some(configuration_pin()),
        );
        assert_eq!(
            status.state,
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(generation),
                reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
            }
        );
        assert_eq!(
            restarted_status, status,
            "status must not synthesize a time-varying activation receipt"
        );
    }

    #[tokio::test]
    async fn activation_without_persisted_scheduler_receipt_is_unavailable() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('a', 'a');
        let generation = published.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        let pin = configuration_pin();
        let command = SemanticActivationCommandV1::new(
            pin,
            SemanticActivationRequestV1::new(generation, None, None).expect("activation request"),
        )
        .expect("activation command");
        let backend = DaemonSemanticRuntimeBackendV1::new(handle);

        assert_eq!(
            backend.activate(&command).await,
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        );
    }

    #[tokio::test]
    async fn compose_application_search_skips_retriever_while_indexing() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('i'),
            2,
            move |progress| async move {
                progress.set_completed_units(1);
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));
        started_rx.await.expect("indexing started");

        struct PanicVectors;
        impl SemanticVectorReadPort for PanicVectors {
            fn scan_exact_flat(
                &self,
                _request: tracedecay_query::retrieval::semantic::SemanticVectorReadRequestV1<'_>,
                _visit: &mut dyn FnMut(
                    &tracedecay_query::retrieval::semantic::SemanticVectorRecordV1,
                ) -> Result<(), RetrievalPortError>,
            ) -> Result<
                tracedecay_query::retrieval::semantic::SemanticVectorScanSummaryV1,
                RetrievalPortError,
            > {
                panic!("indexing composition must not scan vectors")
            }
        }
        struct IdleControl;
        impl SemanticExecutionControl for IdleControl {
            fn is_cancelled(&self) -> bool {
                false
            }
            fn elapsed_micros(&self) -> u64 {
                0
            }
        }

        let authority = tracedecay_semantic::session_pool::test_support::authority();
        let source = source_generation('i');
        let vector = vector_generation('i');
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "compose while indexing",
            SanitizerRevision::try_from("sanitizer.v1".to_owned()).expect("sanitizer"),
            QueryNormalizationRevision::try_from("normalizer.v1".to_owned()).expect("normalizer"),
        )
        .expect("query view");
        let request = composition_request(&query_view, authority.projection(), source, vector);
        let complete = CompleteSemanticGenerationV1::new(
            request.projection.projection_key().clone(),
            request.search_index_key.clone(),
            request.vector_generation.clone(),
            request.code_generation.clone(),
            request.capability_manifest_digest.clone(),
        )
        .expect("complete generation");

        let outcome = compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
            handle: &handle,
            request: &request,
            generation: &complete,
            calibration: None,
            vectors: &PanicVectors,
            control: &IdleControl,
            mode: SemanticQueryModeV1::FallbackAllowed,
            fallback: composition_fallback(),
        })
        .expect("compose while indexing");
        assert!(matches!(
            outcome,
            SemanticQueryServiceOutcomeV1::Fallback { .. }
        ));
        let _ = release_tx.send(());
    }
}
