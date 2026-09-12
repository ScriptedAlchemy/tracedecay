//! Cached vector read ports and semantic search execution.

use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{QueryFallbackSubpayload, SemanticSearchIndexKeyV1};
use tracedecay_query::retrieval::ports::RetrievalExecutionControl;
use tracedecay_query::retrieval::semantic::{
    CompleteSemanticGenerationV1, SemanticCalibrationProfileV1, SemanticIndexStateV1,
    SemanticLaneReadinessV1, SemanticQueryModeV1, SemanticQueryServiceError,
    SemanticQueryServiceOutcomeV1, SemanticRetrievalRequestV1,
};

use super::published_vector_read::{PublishedSemanticVectorReadPortV1, semantic_ann_serving_index};
use super::search_composition::{
    ApplicationSemanticSearchParametersV1, NeverCalledSemanticLane,
    compose_application_semantic_search, execute_calibrated_semantic_query,
};
use super::source_coherence::semantic_source_content_coherent;
use super::{CachedPublishedVectorsV1, ProductionSemanticRuntimeV1, retained_vector_read_port};
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, PublishedVectorGenerationV1, SemanticAnnServingIndexV1,
};

impl ProductionSemanticRuntimeV1 {
    pub(super) fn cached_vector_read_port(
        &self,
        active: PublishedVectorGenerationV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code_generation: &CodeIndexPublishedGenerationV1,
        ann: Option<SemanticAnnServingIndexV1>,
    ) -> Result<Arc<PublishedSemanticVectorReadPortV1>, SemanticQueryServiceError> {
        if let Some(cached) = retained_vector_read_port(
            &self.vector_read_cache,
            active.generation_id(),
            active.projection_key(),
            &search_index_key,
            &code_generation.manifest().generation_id,
            &code_generation.capability().manifest_digest,
        ) {
            return Ok(cached);
        }
        let port = Arc::new(
            PublishedSemanticVectorReadPortV1::new_source_coherent(
                active,
                search_index_key.clone(),
                code_generation,
                ann,
            )
            .map_err(|_| SemanticQueryServiceError::InvalidFallback)?,
        );
        if let Ok(mut guard) = self.vector_read_cache.lock() {
            *guard = Some(CachedPublishedVectorsV1 {
                generation: port.generation.clone(),
                search_index_key,
                source_generation: port.source_generation.clone(),
                port: Arc::clone(&port),
            });
        }
        Ok(port)
    }

    /// Real application consumer for the optional semantic lane. The exact
    /// configuration-pinned generation is loaded before composition; indexing/download never
    /// enters this request path.
    #[hotpath::measure(label = "usecases.semantic.execute_search", future = true)]
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
        C: RetrievalExecutionControl + Sync,
    {
        if request.code_generation == code_generation.manifest().generation_id
            && request.capability_manifest_digest == code_generation.capability().manifest_digest
            && let Some(vectors) = retained_vector_read_port(
                &self.vector_read_cache,
                &request.vector_generation,
                request.projection.projection_key(),
                request.search_index_key,
                &request.code_generation,
                &request.capability_manifest_digest,
            )
        {
            let complete = CompleteSemanticGenerationV1::new(
                request.projection.projection_key().clone(),
                request.search_index_key.clone(),
                request.vector_generation.clone(),
                request.code_generation.clone(),
                request.capability_manifest_digest.clone(),
            )
            .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
            let source_coherence = vectors.source_coherence;
            return compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
                handle: &self.handle,
                request,
                generation: &complete,
                calibration,
                vectors: vectors.as_ref(),
                control,
                mode,
                fallback,
                source_coherence,
            });
        }
        let Ok(retained) = self.graph.graph_for_generation(code_generation).await else {
            return execute_calibrated_semantic_query(
                &NeverCalledSemanticLane,
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        };
        let cancellation = Arc::clone(retained.cancellation());
        let store = match GraphVectorGenerationStoreV1::read_only_generation(
            &retained,
            &request.vector_generation,
        )
        .await
        {
            Ok(Some(store)) => store,
            Ok(None) | Err(_) => {
                return execute_calibrated_semantic_query(
                    &NeverCalledSemanticLane,
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                    mode,
                    fallback,
                );
            }
        };
        let active = match store
            .generation(&request.vector_generation, Arc::clone(&cancellation))
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
        let Some(active) = active else {
            return execute_calibrated_semantic_query(
                &NeverCalledSemanticLane,
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        };
        // The served generation identity is the current publication, and
        // `semantic_source_coherence` is the only authority on whether these
        // vectors may attach to it: the exact source they were projected from,
        // or a publication whose sealed chunk corpus is proven byte-identical
        // (an unrelated republication of the same source truth must not refuse
        // a valid semantic generation). Model/profile identity stays exact
        // through the embedding-key pin; anything unproven fails closed below.
        let generation_id = &code_generation.manifest().generation_id;
        if active.embedding_key() != request.projection
            || request.code_generation != *generation_id
            || !semantic_source_content_coherent(&active, code_generation.manifest())
        {
            return execute_calibrated_semantic_query(
                &NeverCalledSemanticLane,
                SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
                mode,
                fallback,
            );
        }
        let complete = CompleteSemanticGenerationV1::new(
            active.projection_key().clone(),
            request.search_index_key.clone(),
            active.generation_id().clone(),
            generation_id.clone(),
            code_generation.capability().manifest_digest.clone(),
        )
        .map_err(|_| SemanticQueryServiceError::InvalidFallback)?;
        if let Some(field) = complete.mismatch(request) {
            // The service collapses this into `IndexIncompatible`, the same
            // public abstention a failed request contract produces. Name the
            // field and both sides, or a serving-side drift is invisible.
            tracing::warn!(
                event = "semantic_serving_generation_mismatch",
                field,
                request_projection_key = ?request.projection.projection_key(),
                serving_projection_key = ?active.projection_key(),
                request_vector_generation = ?request.vector_generation,
                serving_vector_generation = ?active.generation_id(),
                request_code_generation = %request.code_generation,
                serving_code_generation = %generation_id,
                request_capability_manifest_digest = %request.capability_manifest_digest,
                serving_capability_manifest_digest =
                    %code_generation.capability().manifest_digest,
                "the published semantic generation does not match the pinned request identity"
            );
        }
        let ann = match semantic_ann_serving_index(
            &store,
            &active,
            request.search_index_key,
            Arc::clone(&cancellation),
        )
        .await
        {
            Ok(ann) => ann,
            Err(_) => {
                return execute_calibrated_semantic_query(
                    &NeverCalledSemanticLane,
                    SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Failed),
                    mode,
                    fallback,
                );
            }
        };
        let vectors = self.cached_vector_read_port(
            active,
            request.search_index_key.clone(),
            code_generation,
            ann,
        )?;
        let source_coherence = vectors.source_coherence;
        compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
            handle: &self.handle,
            request,
            generation: &complete,
            calibration,
            vectors: vectors.as_ref(),
            control,
            mode,
            fallback,
            source_coherence,
        })
    }
}
