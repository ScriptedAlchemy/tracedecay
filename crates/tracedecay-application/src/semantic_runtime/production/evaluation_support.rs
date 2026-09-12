//! Evaluation-target admission, calibration, and projection-case helpers for the production runtime.

use std::future::Future;
use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_code_index::projection::expected_request_digest;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkV1,
    ComponentRevision, ManifestDigest, ProjectionBatchRequestV1, ProjectionOperationV1,
    ProjectionReplayReasonV1, SemanticSearchIndexKeyV1, SemanticSearchIndexProfileV1,
    VectorGenerationIdV1, canonical_sha256, sha256_hex_suffix,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;
use tracedecay_query::search_quality::semantic_native::{
    SemanticProjectionCaseOutcomeV1, SemanticProjectionCaseSampleV1,
};
use tracedecay_semantic::projector::PreparedVectorGenerationV1;
use tracedecay_semantic::{
    SemanticEvaluationCancellationV1, SemanticEvaluationProjectionResourcesV1,
    SemanticModelLifecycleOwnerV1,
};
use tracedecay_semantic_contracts::{
    SemanticGenerationPointerV1, SemanticModelLifecycleStateV1, SemanticResourceCeilings,
    SemanticRuntimeScheduleFailureV1,
};

use super::super::graph_provider::RetainedSemanticVectorGraphV1;
use super::super::ports::SemanticRuntimeBackendErrorV1;
use super::SemanticEvaluationLifecycleVerificationV1;
use super::vector_projection_support::commit_evaluation_prepared_generation;
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, SemanticVectorStageDescriptorV1, VectorGenerationPlanV1,
    generation_identity_digest,
};

#[derive(Clone, Copy)]
pub(super) struct InstalledArtifactMemberBytesV1 {
    pub(super) model: u64,
    pub(super) tokenizer: u64,
}

pub(super) fn installed_artifact_member_bytes(
    lifecycle: &SemanticModelLifecycleOwnerV1,
) -> Result<InstalledArtifactMemberBytesV1, SemanticRuntimeScheduleFailureV1> {
    let status = lifecycle.status();
    let state = status
        .state
        .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
    let model = lifecycle
        .catalog()
        .get(state.model_id())
        .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)?;
    let member_bytes = |role: &str| {
        model
            .members
            .get(role)
            .map(|member| member.length)
            .filter(|bytes| *bytes != 0)
            .ok_or(SemanticRuntimeScheduleFailureV1::Artifact)
    };
    Ok(InstalledArtifactMemberBytesV1 {
        model: member_bytes("model")?,
        tokenizer: member_bytes("tokenizer")?,
    })
}

pub(super) fn configured_resource_ceiling_covers(
    configured: &SemanticResourceCeilings,
    required: crate::config::retrieval::SemanticResourceRequirementV1,
) -> bool {
    configured.max_model_bytes >= required.model_bytes
        && configured.max_tokenizer_bytes >= required.tokenizer_bytes
        // An unresolved resident ceiling covers nothing: composition resolves
        // it against the host before this runtime exists, so `None` here means
        // no ceiling was ever admitted, not an unbounded one.
        && configured
            .max_resident_bytes
            .is_some_and(|ceiling| ceiling >= required.resident_bytes)
        && configured.max_threads >= required.threads
        && configured.max_concurrent_sessions >= required.max_concurrent_sessions
        && configured.max_batch_size >= required.batch_size
        && configured.max_sequence_length >= required.sequence_length
        && configured.load_deadline_ms >= required.load_deadline_ms
}

pub(super) fn configured_semantic_resource_ceiling(
    configured: SemanticResourceCeilings,
) -> Result<crate::config::retrieval::SemanticResourceRequirementV1, SemanticRuntimeBackendErrorV1>
{
    Ok(crate::config::retrieval::SemanticResourceRequirementV1 {
        model_bytes: configured.max_model_bytes,
        tokenizer_bytes: configured.max_tokenizer_bytes,
        resident_bytes: resolved_resident_ceiling(configured)?,
        threads: configured.max_threads,
        max_concurrent_sessions: configured.max_concurrent_sessions,
        batch_size: configured.max_batch_size,
        sequence_length: configured.max_sequence_length,
        load_deadline_ms: configured.load_deadline_ms,
    })
}

/// The resident ceiling composition resolved against this host.
///
/// Reading it before that resolution is a refusal rather than a substituted
/// default: every requirement minted from it is compared against a measured
/// evaluation report, so an invented ceiling would be admitted as evidence.
pub(super) fn resolved_resident_ceiling(
    configured: SemanticResourceCeilings,
) -> Result<u64, SemanticRuntimeBackendErrorV1> {
    configured
        .resolved_max_resident_bytes()
        .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)
}

pub(super) fn evaluation_target_resource_requirement(
    configured: SemanticResourceCeilings,
    artifact: InstalledArtifactMemberBytesV1,
) -> Result<crate::config::retrieval::SemanticResourceRequirementV1, SemanticRuntimeBackendErrorV1>
{
    let mut requirement = configured_semantic_resource_ceiling(configured)?;
    requirement.model_bytes = artifact.model;
    requirement.tokenizer_bytes = artifact.tokenizer;
    Ok(requirement)
}

pub(super) fn evaluation_projection_resources(
    configured: SemanticResourceCeilings,
) -> Result<SemanticEvaluationProjectionResourcesV1, SemanticRuntimeScheduleFailureV1> {
    Ok(SemanticEvaluationProjectionResourcesV1 {
        memory_ceiling_bytes: configured
            .resolved_max_resident_bytes()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?,
    })
}

pub(super) fn canonical_exact_flat_search_index_key()
-> Result<SemanticSearchIndexKeyV1, SemanticRuntimeBackendErrorV1> {
    SemanticSearchIndexProfileV1::exact_flat_v1()
        .and_then(|profile| profile.index_key())
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
}

pub(super) fn canonical_ann_hnsw_search_index_key()
-> Result<SemanticSearchIndexKeyV1, SemanticRuntimeBackendErrorV1> {
    SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore_v1()
        .and_then(|profile| profile.index_key())
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
}

/// A pinned key qualifies only when it is one of the canonical profiles this
/// runtime can actually serve: the exact-flat scan, or HNSW candidate
/// generation with exact rescoring (which falls back to the flat scan on a
/// missing or incomplete index).
pub(super) fn validate_evaluation_target_search_index(
    search_index_key: &SemanticSearchIndexKeyV1,
) -> Result<(), SemanticRuntimeBackendErrorV1> {
    if *search_index_key == canonical_exact_flat_search_index_key()?
        || *search_index_key == canonical_ann_hnsw_search_index_key()?
    {
        Ok(())
    } else {
        Err(SemanticRuntimeBackendErrorV1::Rejected)
    }
}

pub(super) const EVALUATION_SEMANTIC_CALIBRATION_COHORT_DOMAIN_V1: &str =
    "tracedecay.semantic.evaluation-calibration-cohort.v1";
pub(super) const EVALUATION_SEMANTIC_MINIMUM_MARGIN_MICROS_V1: u64 = 0;

/// Certify a candidate against the calibration its generation actually
/// measures.
///
/// `measured_maximum_distance_micros` comes from
/// [`measure_acceptance_calibration`] over the committed generation named by
/// `candidate`. Because that measurement is deterministic in an immutable
/// generation, the proposing writer and this certifying reader derive the same
/// bound and exact equality still certifies the candidate.
pub(super) fn certify_evaluation_target_compatibility(
    candidate: &crate::config::retrieval::SemanticCompatibilityPinsV1,
    source_generation: &CodeGenerationId,
    source_manifest_digest: &ManifestDigest,
    capability_manifest_digest: &ManifestDigest,
    measured_maximum_distance_micros: i64,
) -> Result<crate::config::retrieval::SemanticCompatibilityPinsV1, SemanticRuntimeBackendErrorV1> {
    let fusion_revision =
        ComponentRevision::new(tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1)
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    let calibration = canonical_evaluation_calibration(
        candidate,
        source_generation,
        source_manifest_digest,
        capability_manifest_digest,
        measured_maximum_distance_micros,
    )?;
    if candidate.fusion_revision != fusion_revision || candidate.calibration != calibration {
        return Err(SemanticRuntimeBackendErrorV1::Rejected);
    }
    let mut certified = candidate.clone();
    certified.fusion_revision = fusion_revision;
    certified.calibration = calibration;
    Ok(certified)
}

pub(super) fn canonical_evaluation_calibration(
    candidate: &crate::config::retrieval::SemanticCompatibilityPinsV1,
    source_generation: &CodeGenerationId,
    source_manifest_digest: &ManifestDigest,
    capability_manifest_digest: &ManifestDigest,
    measured_maximum_distance_micros: i64,
) -> Result<SemanticCalibrationProfileV1, SemanticRuntimeBackendErrorV1> {
    source_generation
        .validate()
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    source_manifest_digest
        .validate()
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    capability_manifest_digest
        .validate()
        .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    let cohort_digest = canonical_sha256(&(
        EVALUATION_SEMANTIC_CALIBRATION_COHORT_DOMAIN_V1,
        source_generation,
        source_manifest_digest,
        capability_manifest_digest,
        &candidate.projection,
        &candidate.vector_generation_id,
        &candidate.artifact_manifest_digest,
    ))
    .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
    Ok(SemanticCalibrationProfileV1 {
        calibration_profile_id: candidate.calibration.calibration_profile_id.clone(),
        cohort_digest,
        projection_key: candidate.projection.projection_key().clone(),
        vector_generation: candidate.vector_generation_id.clone(),
        capability_manifest_digest: capability_manifest_digest.clone(),
        maximum_distance_micros: measured_maximum_distance_micros,
        minimum_margin_micros: EVALUATION_SEMANTIC_MINIMUM_MARGIN_MICROS_V1,
    })
}

pub(super) fn lifecycle_artifact_matches(
    lifecycle_state: &SemanticModelLifecycleStateV1,
    expected_artifact: &ManifestDigest,
) -> bool {
    let observed = lifecycle_state.artifact_digest();
    observed == expected_artifact.as_str()
        || sha256_hex_suffix(expected_artifact.as_str()) == Some(observed)
}

pub(super) fn check_evaluation_cancellation(
    cancellation: &dyn SemanticEvaluationCancellationV1,
) -> Result<(), SemanticRuntimeBackendErrorV1> {
    match cancellation.interruption() {
        None => Ok(()),
        Some(_) => Err(SemanticRuntimeBackendErrorV1::Unavailable),
    }
}

pub(super) fn revalidation_error(
    error: SemanticRuntimeBackendErrorV1,
) -> SemanticRuntimeBackendErrorV1 {
    match error {
        SemanticRuntimeBackendErrorV1::Unavailable => SemanticRuntimeBackendErrorV1::Unavailable,
        SemanticRuntimeBackendErrorV1::Rejected
        | SemanticRuntimeBackendErrorV1::RejectedAt(_)
        | SemanticRuntimeBackendErrorV1::Conflict => SemanticRuntimeBackendErrorV1::Conflict,
    }
}

pub(super) fn lifecycle_publication_error(
    error: tracedecay_semantic::ModelLifecycleErrorV1,
) -> SemanticRuntimeBackendErrorV1 {
    match error {
        tracedecay_semantic::ModelLifecycleErrorV1::Rejected => {
            SemanticRuntimeBackendErrorV1::Conflict
        }
        tracedecay_semantic::ModelLifecycleErrorV1::Cancelled
        | tracedecay_semantic::ModelLifecycleErrorV1::CancellationCleanupQuarantined(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::CancellationCleanupFailed(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::Catalog(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::StoreUnavailable
        | tracedecay_semantic::ModelLifecycleErrorV1::DownloadFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::DownloadFailedWithReason(_)
        | tracedecay_semantic::ModelLifecycleErrorV1::VerificationFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::RerankerUnavailable
        | tracedecay_semantic::ModelLifecycleErrorV1::InstallFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::WorkerJoinFailed
        | tracedecay_semantic::ModelLifecycleErrorV1::ArtifactImport(_) => {
            SemanticRuntimeBackendErrorV1::Unavailable
        }
    }
}

pub(super) const fn semantic_runtime_backend_outcome(
    error: SemanticRuntimeBackendErrorV1,
) -> &'static str {
    match error {
        SemanticRuntimeBackendErrorV1::Unavailable => "unavailable",
        SemanticRuntimeBackendErrorV1::Rejected | SemanticRuntimeBackendErrorV1::RejectedAt(_) => {
            "rejected"
        }
        SemanticRuntimeBackendErrorV1::Conflict => "conflict",
    }
}

pub(super) fn revalidate_lifecycle_verification(
    expected: &SemanticEvaluationLifecycleVerificationV1,
    observed: &SemanticEvaluationLifecycleVerificationV1,
) -> Result<(), SemanticRuntimeBackendErrorV1> {
    if expected == observed {
        Ok(())
    } else {
        Err(SemanticRuntimeBackendErrorV1::Conflict)
    }
}

pub(super) fn accepted_semantic_resources(
    accepted: crate::config::retrieval::SemanticResourceRequirementV1,
) -> SemanticResourceCeilings {
    SemanticResourceCeilings {
        max_model_bytes: accepted.model_bytes,
        max_tokenizer_bytes: accepted.tokenizer_bytes,
        max_resident_bytes: Some(accepted.resident_bytes),
        max_threads: accepted.threads,
        max_concurrent_sessions: accepted.max_concurrent_sessions,
        max_batch_size: accepted.batch_size,
        max_sequence_length: accepted.sequence_length,
        load_deadline_ms: accepted.load_deadline_ms,
    }
}

pub(super) fn block_on_semantic_evaluation<Output>(
    future: impl Future<Output = Result<Output, SemanticRuntimeScheduleFailureV1>>,
) -> Result<Output, SemanticRuntimeScheduleFailureV1> {
    match tokio::runtime::Handle::try_current() {
        // Daemon evaluation owns a Tokio blocking worker. Those workers retain
        // the runtime handle even though they are outside an async task, so the
        // handle is the canonical executor for projection-case futures.
        Ok(runtime) => runtime.block_on(future),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?
            .block_on(future),
    }
}

pub(super) fn elapsed_micros(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

pub(super) fn semantic_projection_request(
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
        ChangedCodeChunkSetV1 {
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
        }
    };
    // One digest computation serves both branches: it derives the digest of a
    // freshly built full-rebuild change set, and it recomputes an incremental
    // retarget's digest so a malformed source handoff cannot cross the
    // semantic boundary.
    changes.manifest_digest = changes
        .compute_digest()
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
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
    request.request_digest =
        expected_request_digest(&request).map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    Ok(request)
}

pub(super) fn evaluation_projection_plan(
    generation: &CodeIndexPublishedGenerationV1,
    prepared: &PreparedVectorGenerationV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> Result<VectorGenerationPlanV1, SemanticRuntimeScheduleFailureV1> {
    evaluation_projection_plan_from_request(generation, &prepared.request, base_generation)
}

pub(super) fn evaluation_projection_plan_from_request(
    generation: &CodeIndexPublishedGenerationV1,
    request: &ProjectionBatchRequestV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> Result<VectorGenerationPlanV1, SemanticRuntimeScheduleFailureV1> {
    if request.changes.to_generation != generation.manifest().generation_id {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    Ok(evaluation_projection_plan_from_canonical_chunks(
        generation.chunks().chunks(),
        request,
        base_generation,
    ))
}

pub(super) fn evaluation_projection_plan_from_canonical_chunks(
    canonical_chunks: &[Arc<CodeSearchChunkV1>],
    request: &ProjectionBatchRequestV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> VectorGenerationPlanV1 {
    VectorGenerationPlanV1 {
        target_projection_key: request.target_projection_key.clone(),
        source_generation: request.changes.to_generation.clone(),
        source_manifest_digest: request.changes.manifest_digest.clone(),
        expected_chunk_ids: canonical_chunks
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect(),
        base_generation,
    }
}

pub(super) async fn evaluation_projection_case_store(
    retained: &RetainedSemanticVectorGraphV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<GraphVectorGenerationStoreV1, SemanticRuntimeScheduleFailureV1> {
    evaluation_projection_case_store_for_changes(
        retained,
        prepared.embedding_key.clone(),
        &prepared.request.changes,
    )
    .await
}

pub(super) async fn evaluation_projection_case_store_for_changes(
    retained: &RetainedSemanticVectorGraphV1,
    projection: tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    changes: &ChangedCodeChunkSetV1,
) -> Result<GraphVectorGenerationStoreV1, SemanticRuntimeScheduleFailureV1> {
    let store = GraphVectorGenerationStoreV1::open(retained)
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
    let descriptor = SemanticVectorStageDescriptorV1::from_changes(projection, changes)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    store
        .configure_stage(descriptor)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    Ok(store)
}

pub(super) fn evaluation_vector_generation_id(
    generation: &CodeIndexPublishedGenerationV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<VectorGenerationIdV1, SemanticRuntimeScheduleFailureV1> {
    let plan = evaluation_projection_plan(generation, prepared, None)?;
    generation_identity_digest(&plan)
        .map(VectorGenerationIdV1::new)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)
}

pub(super) async fn publish_evaluation_projection_case_isolated(
    store: &GraphVectorGenerationStoreV1,
    cancellation: &Arc<dyn GraphCancellation>,
    generation: &CodeIndexPublishedGenerationV1,
    prepared: &PreparedVectorGenerationV1,
    base_generation: Option<VectorGenerationIdV1>,
) -> Result<
    crate::store::vector_generations::VectorGenerationPublicationV1,
    SemanticRuntimeScheduleFailureV1,
> {
    let plan = evaluation_projection_plan(generation, prepared, base_generation)?;
    let build = store
        .rebuild_generation(plan, Arc::clone(cancellation))
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        .build_id()
        .clone();
    commit_evaluation_prepared_generation(
        store,
        &build,
        prepared,
        generation.chunks().chunks(),
        Arc::clone(cancellation),
    )
    .await?;
    let publication = store
        .publish_generation(&build, Arc::clone(cancellation))
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    if !store
        .published_generation_is_visible(&publication.generation_id, Arc::clone(cancellation))
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
    {
        return Err(SemanticRuntimeScheduleFailureV1::Projection);
    }
    Ok(publication)
}

pub(super) fn projection_case_sample_from_prepared(
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
