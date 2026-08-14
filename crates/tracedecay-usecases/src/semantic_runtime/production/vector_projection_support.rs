use std::sync::Arc;

use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, build_batch_receipt, verify_batch_receipt,
};
use tracedecay_domain::{
    CodeSearchChunkV1, ProjectionBatchRequestV1, ProjectionOperationV1, ProjectionOutcomeV1,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_semantic::SemanticRuntimeScheduleFailureV1;
use tracedecay_semantic::projector::{PreparedVectorGenerationV1, split_projection_request};

use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, VectorGenerationBuildIdV1, VectorGenerationPublicationV1,
    VectorProjectionCheckpointV1,
};

#[derive(Default)]
pub(super) struct BatchCommitStateV1 {
    pub(super) build: Option<VectorGenerationBuildIdV1>,
    pub(super) store: Option<Arc<GraphVectorGenerationStoreV1>>,
    pub(super) checkpoint: Option<VectorProjectionCheckpointV1>,
    pub(super) published: Option<VectorGenerationPublicationV1>,
}

pub(super) fn projection_input_bytes(
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

/// Commit an already-embedded evaluation generation in the same page size
/// production uses. A one-shot corpus commit exceeds the durable stage batch
/// bound (`MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH` / mutation budget).
pub(super) async fn commit_evaluation_prepared_generation(
    store: &GraphVectorGenerationStoreV1,
    build: &VectorGenerationBuildIdV1,
    prepared: PreparedVectorGenerationV1,
    canonical_chunks: &[CodeSearchChunkV1],
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<(), SemanticRuntimeScheduleFailureV1> {
    let pages = split_projection_request(
        &prepared.request,
        canonical_chunks,
        tracedecay_store::MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH,
    )
    .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    let mut checkpoint = None;
    if pages.len() <= 1 {
        store
            .commit_batch(build, None, prepared, cancellation)
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        return Ok(());
    }
    for page in pages {
        let page = evaluation_prepared_page(&prepared, page.request)?;
        checkpoint = Some(
            store
                .commit_batch(build, checkpoint.as_ref(), page, Arc::clone(&cancellation))
                .await
                .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
        );
    }
    Ok(())
}

fn evaluation_prepared_page(
    prepared: &PreparedVectorGenerationV1,
    page_request: ProjectionBatchRequestV1,
) -> Result<PreparedVectorGenerationV1, SemanticRuntimeScheduleFailureV1> {
    let mut vectors = Vec::new();
    let mut tombstones = Vec::new();
    let mut decisions = Vec::new();
    for change in &page_request.changes.added_or_changed {
        let mut vector = prepared
            .vectors
            .iter()
            .find(|vector| vector.chunk_id == change.chunk_id)
            .cloned()
            .ok_or_else(|| {
                SemanticRuntimeScheduleFailureV1::projection(format!(
                    "evaluation page is missing prepared vector {}",
                    change.chunk_id
                ))
            })?;
        vector.source_manifest_digest = page_request.changes.manifest_digest.clone();
        decisions.push(ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: change.current_digest.clone(),
            operation: if change.prior_digest.is_some() {
                ProjectionOperationV1::Updated
            } else {
                ProjectionOperationV1::Added
            },
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: Some(vector.output_digest.clone()),
        });
        vectors.push(vector);
    }
    for change in &page_request.changes.deleted {
        let tombstone = prepared
            .tombstones
            .iter()
            .find(|tombstone| tombstone.chunk_id == change.chunk_id)
            .cloned()
            .ok_or_else(|| {
                SemanticRuntimeScheduleFailureV1::projection(format!(
                    "evaluation page is missing prepared tombstone {}",
                    change.chunk_id
                ))
            })?;
        decisions.push(ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: None,
            operation: ProjectionOperationV1::Deleted,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: None,
        });
        tombstones.push(tombstone);
    }
    for change in &page_request.changes.reused {
        if let Some(mut vector) = prepared
            .vectors
            .iter()
            .find(|vector| vector.chunk_id == change.chunk_id)
            .cloned()
        {
            vector.source_manifest_digest = page_request.changes.manifest_digest.clone();
            decisions.push(ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: ProjectionOperationV1::Updated,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(vector.output_digest.clone()),
            });
            vectors.push(vector);
            continue;
        }
        decisions.push(ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: change.current_digest.clone(),
            operation: ProjectionOperationV1::Reused,
            outcome: ProjectionOutcomeV1::Reused,
            output_digest: None,
        });
    }
    let receipt = build_batch_receipt(&page_request, &decisions)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    verify_batch_receipt(&page_request, &receipt)
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
    Ok(PreparedVectorGenerationV1 {
        embedding_key: prepared.embedding_key.clone(),
        request: page_request,
        receipt,
        vectors,
        tombstones,
    })
}
