//! Production bridge between daemon semantic scheduling and application search.
//!
//! Saved code generations call [`schedule_saved_code_generation`] without waiting
//! for `FastEmbed` download/indexing. Application search admits a semantic lane
//! only through [`query_factory`] once a complete compatible generation is
//! atomically current. Status projection carries indexing progress, degraded
//! reason, and prior generation for Doctor/`tracedecay_runtime`.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CompactCandidate,
    ComponentRevision, EvidenceRole, FixedPointScore, LogicalEvidenceId, ManifestDigest,
    Pr9FallbackSubpayload, ProjectionBatchRequestV1, ProjectionReplayReasonV1, RetrievalAnchorId,
    RetrieverBatch, RetrieverKind, RetrieverOutcome, ScoreDomainId, SourceOccurrenceId, UtcMicros,
    VectorGenerationIdV1, canonical_sha256,
};

use crate::code_index::production::CodeIndexPublishedGenerationV1;
use crate::code_index::projection::expected_request_digest;
use crate::config::SemanticResourceCeilings;
use crate::db::Database;
use crate::query::retrieval::graph::production_code_index_freshness;
use crate::query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, RetrievalPortError,
};
use crate::query::retrieval::semantic::{
    CalibratedSemanticQueryService, CodeSemanticEvidenceV1, CompleteSemanticGenerationV1,
    SemanticCalibrationProfileV1, SemanticCodeRetriever, SemanticExecutionControl,
    SemanticIndexStateV1, SemanticLaneReadinessV1, SemanticLaneRetriever, SemanticQueryModeV1,
    SemanticQueryServiceError, SemanticQueryServiceOutcomeV1, SemanticRetrievalRequestV1,
    SemanticSearchKindV1, SemanticVectorReadPort, SemanticVectorReadRequestV1,
    SemanticVectorRecordV1, SemanticVectorScanSummaryV1,
};
#[cfg(test)]
use crate::semantic_code::DaemonSemanticQueryFactoryV1;
use crate::semantic_code::projector::PreparedVectorGenerationV1;
use crate::semantic_code::{
    DaemonSemanticRuntimeHandleV1, FastEmbedSemanticGenerationRequestV1, LoadedSemanticArtifactV1,
    PreparedSemanticRuntimeCommitV1, SemanticGenerationPointerV1, SemanticModelLifecycleOwnerV1,
    SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
    SemanticRuntimeStatusProjectionV1,
};
use crate::store::vector_generations::{
    DatabaseVectorGenerationStoreV1, PublishedVectorGenerationV1, VectorGenerationPlanV1,
};

use super::ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticActivationRequestV1,
    SemanticConfigurationPinV1, SemanticFallbackReasonV1, SemanticRollbackCommandV1,
    SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1,
    SemanticRuntimeFuture, SemanticRuntimeStateV1, SemanticRuntimeStatusV1,
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
            match configuration
                .as_ref()
                .and_then(|pin| synthesize_current_receipt(pin, generation))
            {
                Some(receipt) => SemanticRuntimeStateV1::Current { receipt },
                None => SemanticRuntimeStateV1::Degraded {
                    active_generation: Some(generation.clone()),
                    reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
                },
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
    lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    resources: SemanticResourceCeilings,
}

impl ProductionSemanticRuntimeV1 {
    pub fn new(
        handle: DaemonSemanticRuntimeHandleV1,
        database: Arc<Database>,
        lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
        resources: SemanticResourceCeilings,
    ) -> Self {
        Self {
            handle,
            database,
            lifecycle,
            resources,
        }
    }

    /// Restore a compatible immutable generation after daemon restart.
    pub async fn restore_current(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<bool, SemanticRuntimeScheduleFailureV1> {
        let store = DatabaseVectorGenerationStoreV1::open(self.database.as_ref())
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let Some(active) = store
            .active_generation()
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?
        else {
            return Ok(false);
        };
        if active.source_generation() != &generation.manifest().generation_id {
            return Ok(false);
        }
        let lifecycle = Arc::clone(&self.lifecycle);
        let manifest = generation.manifest().clone();
        let resources = self.resources;
        let artifact = tokio::task::spawn_blocking(move || {
            LoadedSemanticArtifactV1::from_lifecycle(&lifecycle, &manifest, resources)
        })
        .await
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)??;
        if artifact.projection() != active.embedding_key() {
            return Ok(false);
        }
        self.handle.restore_current(
            SemanticGenerationPointerV1 {
                generation: active.generation_id().clone(),
                source_generation: active.source_generation().clone(),
                projection_key: active.projection_key().clone(),
            },
            artifact,
        )?;
        let _ = self.lifecycle.mark_ready();
        Ok(true)
    }

    /// Enqueue one saved code generation. Model verification, ORT startup,
    /// changed-chunk embedding, and database publication remain background work.
    pub fn schedule_saved_generation(&self, generation: &CodeIndexPublishedGenerationV1) -> bool {
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
                    |_prepared| async move { Err(SemanticRuntimeScheduleFailureV1::Publication) },
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
                let store = DatabaseVectorGenerationStoreV1::open(database.as_ref())
                    .await
                    .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                let plan = VectorGenerationPlanV1 {
                    target_projection_key: prepared.request.target_projection_key.clone(),
                    source_generation: prepared.request.changes.to_generation.clone(),
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
                    let store = DatabaseVectorGenerationStoreV1::open(database_for_commit.as_ref())
                        .await
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                    let publication = store
                        .publish_generation(&build, expected_active.as_ref())
                        .await
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
                    let active = store
                        .generation(&publication.generation_id)
                        .await
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?
                        .ok_or(SemanticRuntimeScheduleFailureV1::Publication)?;
                    let _ = lifecycle_for_commit.mark_ready();
                    Ok(SemanticGenerationPointerV1 {
                        generation: publication.generation_id,
                        source_generation: active.source_generation().clone(),
                        projection_key: active.projection_key().clone(),
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
        fallback: Arc<Pr9FallbackSubpayload>,
    ) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
    where
        C: SemanticExecutionControl,
    {
        let store = match DatabaseVectorGenerationStoreV1::open(self.database.as_ref()).await {
            Ok(store) => store,
            Err(_) => {
                return CalibratedSemanticQueryService::new(&NeverCalledSemanticLane).execute(
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Failed),
                    mode,
                    fallback,
                );
            }
        };
        let active = match store.active_generation().await {
            Ok(active) => active,
            Err(_) => {
                return CalibratedSemanticQueryService::new(&NeverCalledSemanticLane).execute(
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Failed),
                    mode,
                    fallback,
                );
            }
        };
        let Some(active) = active else {
            return CalibratedSemanticQueryService::new(&NeverCalledSemanticLane).execute(
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        };
        let complete = CompleteSemanticGenerationV1::new(
            active.projection_key().clone(),
            active.generation_id().clone(),
            active.source_generation().clone(),
            code_generation.capability().manifest_digest.clone(),
        )
        .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
        let vectors = PublishedSemanticVectorReadPortV1::new(active, code_generation)
            .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
        compose_application_semantic_search(
            &self.handle,
            request,
            &complete,
            calibration,
            &vectors,
            control,
            mode,
            fallback,
        )
    }

    pub async fn rollback(
        &self,
        target: &VectorGenerationIdV1,
        expected_active: &VectorGenerationIdV1,
    ) -> Result<SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1> {
        let store = DatabaseVectorGenerationStoreV1::open(self.database.as_ref())
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let generation = store
            .generation(target)
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
        let publication = store
            .activate_generation(target, Some(expected_active))
            .await
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
        let pointer = SemanticGenerationPointerV1 {
            generation: publication.generation_id,
            source_generation: generation.source_generation().clone(),
            projection_key: generation.projection_key().clone(),
        };
        self.handle.restore_current(pointer.clone(), artifact)?;
        let _ = self.lifecycle.mark_ready();
        Ok(pointer)
    }
}

struct PublishedSemanticVectorReadPortV1 {
    generation: VectorGenerationIdV1,
    projection_key: tracedecay_domain::ProjectionKeyV1,
    source_generation: CodeGenerationId,
    capability_manifest_digest: ManifestDigest,
    rows: Vec<SemanticVectorRecordV1>,
}

impl PublishedSemanticVectorReadPortV1 {
    fn new(
        vectors: PublishedVectorGenerationV1,
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
            let anchor_id = RetrievalAnchorId::new(format!("code-chunk:{}", chunk_id.as_str()))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
            let source_occurrence =
                SourceOccurrenceId::new(format!("code-chunk:{}", chunk_id.as_str()))
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
            let candidate = CompactCandidate {
                anchor_id: anchor_id.clone(),
                logical_evidence_id: LogicalEvidenceId::new(format!(
                    "code-chunk:{}",
                    chunk_id.as_str()
                ))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                source_occurrence_id: source_occurrence.clone(),
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
#[cfg(test)]
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

/// Application search composition: admit `SemanticCodeRetriever` only through
/// [`DaemonSemanticRuntimeHandleV1::query_factory`].
///
/// Non-ready / indexing / degraded states never construct the retriever and
/// return the frozen PR9 fallback without waiting on `FastEmbed` download or
/// projection. Exact/lexical/graph owners stay independently callable.
pub fn compose_application_semantic_search<'a, V, C>(
    handle: &DaemonSemanticRuntimeHandleV1,
    request: &'a SemanticRetrievalRequestV1<'a>,
    generation: &'a CompleteSemanticGenerationV1,
    calibration: Option<&'a SemanticCalibrationProfileV1>,
    vectors: &'a V,
    control: &'a C,
    mode: SemanticQueryModeV1,
    fallback: Arc<Pr9FallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    V: SemanticVectorReadPort,
    C: SemanticExecutionControl,
{
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
                return CalibratedSemanticQueryService::new(&NeverCalledSemanticLane).execute(
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Incompatible),
                    mode,
                    fallback,
                );
            };
            let embedder = factory.create(Arc::new(|| false));
            let lane = SemanticCodeRetriever::new(&embedder, vectors, control);
            CalibratedSemanticQueryService::new(&lane).execute(
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
            CalibratedSemanticQueryService::new(&NeverCalledSemanticLane).execute(
                unavailable,
                mode,
                fallback,
            )
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
    fallback: Arc<Pr9FallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    C: SemanticExecutionControl,
{
    let Some(runtime) = project_semantic_production_runtime(project_root) else {
        return CalibratedSemanticQueryService::new(&NeverCalledSemanticLane).execute(
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
            let Some(current) = self.handle.current() else {
                return Err(SemanticRuntimeBackendErrorV1::Unavailable);
            };
            if current.generation != command.request.target_generation {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            SemanticActivationReceiptV1::issue(command, now_micros())
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
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
            runtime
                .rollback(
                    &command.request.target_generation,
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

fn synthesize_current_receipt(
    configuration: &SemanticConfigurationPinV1,
    generation: &VectorGenerationIdV1,
) -> Option<SemanticActivationReceiptV1> {
    let request = SemanticActivationRequestV1::new(generation.clone(), None, None).ok()?;
    let command = SemanticActivationCommandV1::new(configuration.clone(), request).ok()?;
    SemanticActivationReceiptV1::issue(&command, now_micros()).ok()
}

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros() as i64);
    UtcMicros(micros)
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

/// Application status for a mounted project semantic scheduler, if any.
pub fn project_semantic_application_status(project_root: &Path) -> Option<SemanticRuntimeStatusV1> {
    if let Some(runtime) = project_semantic_production_runtime(project_root) {
        return Some(DaemonSemanticRuntimeBackendV1::from_production(runtime).application_status());
    }
    let handle = project_semantic_handles()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()?;
    Some(application_status_from_projection(
        &handle.status_projection(),
        None,
    ))
}

/// Hook invoked after a code generation publishes; must not block search.
pub type SavedCodeGenerationScheduleHookV1 =
    Arc<dyn Fn(&CodeIndexPublishedGenerationV1) -> bool + Send + Sync>;

/// Production hook: enqueue semantic projection for each saved generation.
///
/// Artifact admission remains owned by the model lifecycle. Until a complete
/// compatible artifact is available the background task fails closed without
/// joining into exact/lexical/graph search.
pub fn production_saved_generation_schedule_hook(
    project_root: PathBuf,
    handle: DaemonSemanticRuntimeHandleV1,
    database: Arc<Database>,
    lifecycle: Arc<SemanticModelLifecycleOwnerV1>,
    resources: SemanticResourceCeilings,
) -> SavedCodeGenerationScheduleHookV1 {
    let runtime = Arc::new(ProductionSemanticRuntimeV1::new(
        handle, database, lifecycle, resources,
    ));
    project_semantic_production_runtimes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root, runtime.as_ref().clone());
    Arc::new(move |generation| {
        let runtime = Arc::clone(&runtime);
        let generation = generation.clone();
        let Ok(tokio) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        tokio.spawn(async move {
            match runtime.restore_current(&generation).await {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    let _ = runtime.schedule_saved_generation(&generation);
                }
            }
        });
        true
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use tokio::sync::oneshot;
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, CodeGenerationId, CodeSearchChunkV1, ManifestDigest,
        ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionReplayReasonV1,
        VectorGenerationIdV1,
    };

    use crate::semantic_code::{
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
            ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
                .expect("manifest digest"),
        )
    }

    fn projection_key() -> ProjectionKeyV1 {
        ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: "embedding.test.v1".to_owned(),
            profile_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                .expect("projection profile digest"),
        }
    }

    fn pointer(vector: char, source: char) -> SemanticGenerationPointerV1 {
        SemanticGenerationPointerV1 {
            generation: vector_generation(vector),
            source_generation: source_generation(source),
            projection_key: projection_key(),
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
                crate::semantic_code::session_pool::tests::authority(),
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
            crate::application::semantic_runtime::SemanticRuntimeRouteV1::LexicalFallback { .. }
        ));
    }

    #[tokio::test]
    async fn compose_application_search_skips_retriever_while_indexing() {
        use std::collections::BTreeMap;
        use tracedecay_domain::{
            AuthorizationRevision, EphemeralSanitizedQueryViewV1, FallbackSubpayloadDigest,
            FusionProfileId, PrincipalId, PublicRetrieverStatus, QueryDigest, QueryMac,
            QueryNormalizationRevision, RepositoryId, RetrievalRequest, RetrievalScope,
            RetrievalSnapshot, RetrieverKind, SanitizerRevision, SingleRootScopeV1, TemporalModeV1,
            VectorWatermark,
        };

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
                _request: crate::query::retrieval::semantic::SemanticVectorReadRequestV1<'_>,
                _visit: &mut dyn FnMut(
                    &crate::query::retrieval::semantic::SemanticVectorRecordV1,
                ) -> Result<(), RetrievalPortError>,
            ) -> Result<
                crate::query::retrieval::semantic::SemanticVectorScanSummaryV1,
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

        let authority = crate::semantic_code::session_pool::tests::authority();
        let source = source_generation('i');
        let vector = vector_generation('i');
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "compose while indexing",
            SanitizerRevision::try_from("sanitizer.v1".to_owned()).expect("sanitizer"),
            QueryNormalizationRevision::try_from("normalizer.v1".to_owned()).expect("normalizer"),
        )
        .expect("query view");
        let query_digest = QueryDigest::new(
            authority.projection().privacy_domain().clone(),
            authority.projection().privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "33".repeat(32))).expect("mac"),
        );
        let budget = tracedecay_domain::RetrievalBudget {
            max_candidates_per_lane: 8,
            max_fused_candidates: 16,
            max_hydrated_results: 8,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        };
        let request = SemanticRetrievalRequestV1 {
            base: RetrievalRequest {
                principal: PrincipalId::try_from("principal.fixture".to_owned())
                    .expect("principal"),
                scope: RetrievalScope {
                    privacy_domain: authority.projection().privacy_domain().clone(),
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
            query_view: &query_view,
            projection: authority.projection(),
            capability_manifest_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("capability"),
            vector_generation: vector.clone(),
            code_generation: source.clone(),
            budget,
        };
        let complete = CompleteSemanticGenerationV1::new(
            authority.projection().projection_key().clone(),
            vector,
            source,
            request.capability_manifest_digest.clone(),
        )
        .expect("complete generation");
        let mut fallback = Pr9FallbackSubpayload {
            profile_id: FusionProfileId::try_from("profile.pr9.semantic-contract.v1".to_owned())
                .expect("profile"),
            ordered_candidates: Vec::new(),
            public_pr9_lane_coverage: BTreeMap::from([
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

        let outcome = compose_application_semantic_search(
            &handle,
            &request,
            &complete,
            None,
            &PanicVectors,
            &IdleControl,
            SemanticQueryModeV1::FallbackAllowed,
            Arc::new(fallback),
        )
        .expect("compose while indexing");
        assert!(matches!(
            outcome,
            SemanticQueryServiceOutcomeV1::Fallback { .. }
        ));
        let _ = release_tx.send(());
    }
}
