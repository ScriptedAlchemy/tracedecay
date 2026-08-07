use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use tracedecay_domain::{CodeSearchChunkId, VectorGenerationIdV1};
use tracedecay_graph_db::{GraphCancellation, VectorSearchRequest};

use super::native_records::{generation_vector_entity_id, read_generation_metadata};
use super::persistence::{
    check_cancelled, map_graph_error, normalized_vector_score, required_string,
    search_vector_property, storage_error, vector_metric,
};
use super::snapshot::SemanticVectorVerifiedRead;
use super::{
    CHUNK_ID_PROPERTY, DeadlineGraphCancellationV1, GENERATION_ID_PROPERTY,
    GraphVectorGenerationStoreV1, MAX_SEMANTIC_HYBRID_LEXICAL_CANDIDATES,
    MAX_SEMANTIC_VECTOR_SEARCH_RESULTS, SemanticHybridGraphMatchV1,
    SemanticHybridGraphSearchRequestV1, SemanticHybridGraphSearchResultV1,
    SemanticVectorGraphMatchV1, SemanticVectorGraphSearchRequestV1,
    SemanticVectorGraphSearchResultV1, VectorGenerationStoreErrorV1,
};

impl GraphVectorGenerationStoreV1 {
    pub async fn search_vectors(
        &self,
        request: SemanticVectorGraphSearchRequestV1,
    ) -> Result<SemanticVectorGraphSearchResultV1, VectorGenerationStoreErrorV1> {
        check_cancelled(request.cancellation.as_ref())?;
        let snapshot = self.snapshot()?;
        self.search_vectors_in_snapshot(&snapshot, request)
    }

    fn search_vectors_in_snapshot(
        &self,
        snapshot: &SemanticVectorVerifiedRead,
        request: SemanticVectorGraphSearchRequestV1,
    ) -> Result<SemanticVectorGraphSearchResultV1, VectorGenerationStoreErrorV1> {
        check_search_control(&request)?;
        if request.limit == 0 || request.limit > MAX_SEMANTIC_VECTOR_SEARCH_RESULTS {
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "semantic vector search limit exceeds the bounded native result budget".to_owned(),
            ));
        }
        let generation = read_generation_metadata(
            snapshot,
            &request.generation_id,
            Arc::clone(&request.cancellation),
        )?
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        if generation.embedding_key != request.embedding_key
            || generation.source_generation != request.source_generation
            || generation.source_manifest_digest != request.source_manifest_digest
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }
        let embedding = request.embedding_key.embedding_key();
        let expected_dimension = usize::try_from(embedding.dimensions).map_err(storage_error)?;
        if request.query.len() != expected_dimension
            || request.query.iter().any(|value| !value.is_finite())
        {
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "semantic vector query shape does not match its projection".to_owned(),
            ));
        }
        let bounded_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(DeadlineGraphCancellationV1 {
                request: Arc::clone(&request.cancellation),
                deadline: request.deadline,
            });
        let native = snapshot
            .vector_search(VectorSearchRequest {
                namespace: snapshot.projection().namespace.clone(),
                projection: snapshot.projection().projection.clone(),
                property: search_vector_property(&request.generation_id)?,
                query: request.query.clone(),
                dimension: expected_dimension,
                metric: vector_metric(embedding.metric),
                limit: request.limit,
                cancellation: Arc::clone(&bounded_cancellation),
            })
            .map_err(|error| {
                if Instant::now() >= request.deadline {
                    VectorGenerationStoreErrorV1::DeadlineExceeded
                } else {
                    map_graph_error(error)
                }
            })?;
        let mut matches = Vec::with_capacity(native.matches.len());
        for candidate in native.matches {
            check_search_control(&request)?;
            let entity = snapshot
                .entity(
                    &snapshot.projection().namespace,
                    &candidate.entity,
                    Arc::clone(&bounded_cancellation),
                )
                .map_err(map_graph_error)?
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Corrupt(
                        "native semantic vector match is missing its entity".to_owned(),
                    )
                })?;
            if required_string(&entity, GENERATION_ID_PROPERTY)?
                != request.generation_id.as_digest().as_str()
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector search returned a foreign generation row".to_owned(),
                ));
            }
            let chunk_id = CodeSearchChunkId::try_from(
                required_string(&entity, CHUNK_ID_PROPERTY)?.to_owned(),
            )
            .map_err(storage_error)?;
            matches.push(SemanticVectorGraphMatchV1 {
                chunk_id,
                distance: candidate.distance,
            });
        }
        sort_vector_matches(&mut matches);
        check_search_control(&request)?;
        Ok(SemanticVectorGraphSearchResultV1 {
            generation_id: request.generation_id,
            matches,
        })
    }

    pub async fn search_hybrid(
        &self,
        request: SemanticHybridGraphSearchRequestV1,
    ) -> Result<SemanticHybridGraphSearchResultV1, VectorGenerationStoreErrorV1> {
        if request.limit == 0
            || request.limit > MAX_SEMANTIC_VECTOR_SEARCH_RESULTS
            || request.lexical.len() > MAX_SEMANTIC_HYBRID_LEXICAL_CANDIDATES
            || !request.vector_weight.is_finite()
            || !request.lexical_weight.is_finite()
            || request.vector_weight < 0.0
            || request.lexical_weight < 0.0
            || request.vector_weight + request.lexical_weight <= 0.0
            || request
                .lexical
                .iter()
                .any(|candidate| !candidate.score.is_finite() || candidate.score < 0.0)
        {
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "semantic hybrid search weights, scores, and limit must be finite and positive"
                    .to_owned(),
            ));
        }
        let cancellation = Arc::clone(&request.vector.cancellation);
        let deadline = request.vector.deadline;
        let generation_id = request.vector.generation_id.clone();
        let snapshot = self.snapshot()?;
        let vector = self.search_vectors_in_snapshot(&snapshot, request.vector)?;
        let lexical_chunks = request
            .lexical
            .iter()
            .map(|candidate| candidate.chunk_id.clone())
            .collect::<BTreeSet<_>>();
        let eligible = self.generation_chunk_ids(
            &snapshot,
            &generation_id,
            &lexical_chunks,
            Arc::new(DeadlineGraphCancellationV1 {
                request: Arc::clone(&cancellation),
                deadline,
            }),
            deadline,
        )?;
        let lexical_max = request
            .lexical
            .iter()
            .filter(|candidate| eligible.contains(&candidate.chunk_id))
            .map(|candidate| candidate.score)
            .max_by(f64::total_cmp)
            .unwrap_or(0.0);
        let mut fused = BTreeMap::<CodeSearchChunkId, SemanticHybridGraphMatchV1>::new();
        for candidate in vector.matches {
            check_search_deadline(cancellation.as_ref(), deadline)?;
            let score = normalized_vector_score(candidate.distance);
            fused.insert(
                candidate.chunk_id.clone(),
                SemanticHybridGraphMatchV1 {
                    chunk_id: candidate.chunk_id,
                    vector_distance: Some(candidate.distance),
                    lexical_score: None,
                    combined_score: request.vector_weight * score,
                },
            );
        }
        for candidate in request.lexical {
            check_search_deadline(cancellation.as_ref(), deadline)?;
            if !eligible.contains(&candidate.chunk_id) {
                continue;
            }
            let normalized = if lexical_max == 0.0 {
                0.0
            } else {
                candidate.score / lexical_max
            };
            let entry = fused.entry(candidate.chunk_id.clone()).or_insert_with(|| {
                SemanticHybridGraphMatchV1 {
                    chunk_id: candidate.chunk_id.clone(),
                    vector_distance: None,
                    lexical_score: None,
                    combined_score: 0.0,
                }
            });
            if entry
                .lexical_score
                .is_none_or(|prior| candidate.score > prior)
            {
                if let Some(prior) = entry.lexical_score {
                    entry.combined_score -= request.lexical_weight
                        * if lexical_max == 0.0 {
                            0.0
                        } else {
                            prior / lexical_max
                        };
                }
                entry.lexical_score = Some(candidate.score);
                entry.combined_score += request.lexical_weight * normalized;
            }
        }
        check_search_deadline(cancellation.as_ref(), deadline)?;
        let mut matches = fused.into_values().collect::<Vec<_>>();
        sort_hybrid_matches(&mut matches);
        matches.truncate(request.limit);
        check_search_deadline(cancellation.as_ref(), deadline)?;
        Ok(SemanticHybridGraphSearchResultV1 {
            generation_id,
            matches,
        })
    }

    fn generation_chunk_ids(
        &self,
        snapshot: &SemanticVectorVerifiedRead,
        generation_id: &VectorGenerationIdV1,
        candidates: &BTreeSet<CodeSearchChunkId>,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> Result<BTreeSet<CodeSearchChunkId>, VectorGenerationStoreErrorV1> {
        let mut chunks = BTreeSet::new();
        for candidate in candidates {
            check_search_deadline(cancellation.as_ref(), deadline)?;
            let Some(entity) = snapshot
                .entity(
                    &snapshot.projection().namespace,
                    &generation_vector_entity_id(generation_id, candidate)?,
                    Arc::clone(&cancellation),
                )
                .map_err(map_graph_error)?
            else {
                continue;
            };
            if required_string(&entity, GENERATION_ID_PROPERTY)?
                != generation_id.as_digest().as_str()
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector search row names a foreign generation".to_owned(),
                ));
            }
            let chunk_id = CodeSearchChunkId::try_from(
                required_string(&entity, CHUNK_ID_PROPERTY)?.to_owned(),
            )
            .map_err(storage_error)?;
            if &chunk_id != candidate {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector membership row is bound to a foreign chunk".to_owned(),
                ));
            }
            if !chunks.insert(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector generation contains duplicate search chunks".to_owned(),
                ));
            }
        }
        Ok(chunks)
    }
}

fn sort_vector_matches(matches: &mut [SemanticVectorGraphMatchV1]) {
    matches.sort_by(|left, right| {
        left.distance
            .total_cmp(&right.distance)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
}

fn sort_hybrid_matches(matches: &mut [SemanticHybridGraphMatchV1]) {
    matches.sort_by(|left, right| {
        right
            .combined_score
            .total_cmp(&left.combined_score)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
}

fn check_search_control(
    request: &SemanticVectorGraphSearchRequestV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if request.cancellation.is_cancelled() {
        Err(VectorGenerationStoreErrorV1::Cancelled)
    } else if Instant::now() >= request.deadline {
        Err(VectorGenerationStoreErrorV1::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn check_search_deadline(
    cancellation: &dyn GraphCancellation,
    deadline: Instant,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if cancellation.is_cancelled() {
        Err(VectorGenerationStoreErrorV1::Cancelled)
    } else if Instant::now() >= deadline {
        Err(VectorGenerationStoreErrorV1::DeadlineExceeded)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(value: &str) -> CodeSearchChunkId {
        CodeSearchChunkId::new(value).expect("chunk id")
    }

    #[test]
    fn vector_and_hybrid_ties_are_ordered_by_canonical_chunk_identity() {
        let mut vector = vec![
            SemanticVectorGraphMatchV1 {
                chunk_id: chunk("chunk.z"),
                distance: 0.25,
            },
            SemanticVectorGraphMatchV1 {
                chunk_id: chunk("chunk.a"),
                distance: 0.25,
            },
        ];
        sort_vector_matches(&mut vector);
        assert_eq!(vector[0].chunk_id, chunk("chunk.a"));

        let mut hybrid = vec![
            SemanticHybridGraphMatchV1 {
                chunk_id: chunk("chunk.z"),
                vector_distance: Some(0.25),
                lexical_score: Some(1.0),
                combined_score: 0.75,
            },
            SemanticHybridGraphMatchV1 {
                chunk_id: chunk("chunk.a"),
                vector_distance: Some(0.25),
                lexical_score: Some(1.0),
                combined_score: 0.75,
            },
        ];
        sort_hybrid_matches(&mut hybrid);
        assert_eq!(hybrid[0].chunk_id, chunk("chunk.a"));
    }
}
