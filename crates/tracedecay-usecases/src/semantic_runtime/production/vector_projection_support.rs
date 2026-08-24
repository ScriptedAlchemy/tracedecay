use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, build_batch_receipt, verify_batch_receipt,
};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeSearchChunkId, CodeSearchChunkV1,
    ProjectionBatchRequestV1, ProjectionOperationV1, ProjectionOutcomeV1,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_semantic::SemanticRuntimeScheduleFailureV1;
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, VectorTombstoneV1, split_projection_request,
};

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
        prepared.embedding_key.embedding_key().inference_batch_size as usize,
        prepared.embedding_key.embedding_key().inference_batch_bytes as usize,
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
    let mut prepared = EvaluationPreparedPageIndexV1::new(prepared)?;
    let mut prepared_pages = Vec::with_capacity(pages.len());
    for page in pages {
        prepared_pages.push(evaluation_prepared_page(&mut prepared, page.request)?);
    }
    prepared.finish()?;
    for page in prepared_pages {
        checkpoint = Some(
            store
                .commit_batch(build, checkpoint.as_ref(), page, Arc::clone(&cancellation))
                .await
                .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
        );
    }
    Ok(())
}

struct EvaluationPreparedPageIndexV1 {
    embedding_key: AdmittedEmbeddingProjectionKeyV1,
    vectors: BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
    tombstones: BTreeMap<CodeSearchChunkId, VectorTombstoneV1>,
}

impl EvaluationPreparedPageIndexV1 {
    fn new(prepared: PreparedVectorGenerationV1) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let mut vectors = BTreeMap::new();
        for vector in prepared.vectors {
            let chunk_id = vector.chunk_id.clone();
            if vectors.insert(chunk_id.clone(), vector).is_some() {
                return Err(SemanticRuntimeScheduleFailureV1::projection(format!(
                    "evaluation paging received duplicate prepared vector {chunk_id}"
                )));
            }
        }
        let mut tombstones = BTreeMap::new();
        for tombstone in prepared.tombstones {
            let chunk_id = tombstone.chunk_id.clone();
            if tombstones.insert(chunk_id.clone(), tombstone).is_some() {
                return Err(SemanticRuntimeScheduleFailureV1::projection(format!(
                    "evaluation paging received duplicate prepared tombstone {chunk_id}"
                )));
            }
        }
        Ok(Self {
            embedding_key: prepared.embedding_key,
            vectors,
            tombstones,
        })
    }

    fn finish(self) -> Result<(), SemanticRuntimeScheduleFailureV1> {
        if let Some(chunk_id) = self.vectors.keys().next() {
            return Err(SemanticRuntimeScheduleFailureV1::projection(format!(
                "evaluation paging retained an unrequested prepared vector {chunk_id}"
            )));
        }
        if let Some(chunk_id) = self.tombstones.keys().next() {
            return Err(SemanticRuntimeScheduleFailureV1::projection(format!(
                "evaluation paging retained an unrequested prepared tombstone {chunk_id}"
            )));
        }
        Ok(())
    }
}

fn evaluation_prepared_page(
    prepared: &mut EvaluationPreparedPageIndexV1,
    page_request: ProjectionBatchRequestV1,
) -> Result<PreparedVectorGenerationV1, SemanticRuntimeScheduleFailureV1> {
    let mut vectors = Vec::new();
    let mut tombstones = Vec::new();
    let mut decisions = Vec::new();
    for change in &page_request.changes.added_or_changed {
        let mut vector = prepared.vectors.remove(&change.chunk_id).ok_or_else(|| {
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
            .remove(&change.chunk_id)
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
        if let Some(mut vector) = prepared.vectors.remove(&change.chunk_id) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_code_index::projection::expected_request_digest;
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision, CodeGenerationId,
        ContentDigest, EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1,
        EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
        EmbeddingTruncationSideV1, ManifestDigest, PrivacyDomainId, ProjectionReplayReasonV1,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical fixture identity")
    }

    fn digest(byte: char) -> ManifestDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn content_digest(byte: char) -> ContentDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn embedding() -> AdmittedEmbeddingProjectionKeyV1 {
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: digest('1'),
            tokenizer_digest: digest('2'),
            config_digest: digest('3'),
            query_instruction_digest: Some(digest('4')),
            document_instruction_digest: Some(digest('5')),
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            inference_batch_size: 8,
            inference_batch_bytes: 16 * 1024,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "paging-fixture.v1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 1,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            privacy_domain: id::<PrivacyDomainId>("privacy.paging-fixture"),
            privacy_key_epoch: 1,
        }
        .admit()
        .expect("admitted paging fixture")
    }

    fn request(
        added: &[(&str, Option<char>, char)],
        deleted: &[(&str, char)],
        reused: &[(&str, char)],
    ) -> ProjectionBatchRequestV1 {
        let to_generation = id::<CodeGenerationId>("generation.paging-target");
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: Some(id("generation.paging-base")),
            to_generation,
            manifest_digest: digest('a'),
            added_or_changed: added
                .iter()
                .map(|(chunk_id, prior, current)| ChangedCodeChunkV1 {
                    chunk_id: id(chunk_id),
                    prior_digest: prior.map(content_digest),
                    current_digest: Some(content_digest(*current)),
                })
                .collect(),
            deleted: deleted
                .iter()
                .map(|(chunk_id, prior)| ChangedCodeChunkV1 {
                    chunk_id: id(chunk_id),
                    prior_digest: Some(content_digest(*prior)),
                    current_digest: None,
                })
                .collect(),
            reused: reused
                .iter()
                .map(|(chunk_id, current)| ChangedCodeChunkV1 {
                    chunk_id: id(chunk_id),
                    prior_digest: Some(content_digest(*current)),
                    current_digest: Some(content_digest(*current)),
                })
                .collect(),
        };
        changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
        let projection = embedding();
        let mut request = ProjectionBatchRequestV1 {
            request_digest: changes.manifest_digest.clone(),
            changes,
            previous_projection_key: Some(projection.projection_key().clone()),
            target_projection_key: projection.projection_key().clone(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        };
        request.request_digest = expected_request_digest(&request).expect("request digest");
        request
    }

    fn vector(
        projection: &AdmittedEmbeddingProjectionKeyV1,
        request: &ProjectionBatchRequestV1,
        chunk_id: &str,
        chunk_digest: char,
        value: f32,
    ) -> ProjectedChunkVectorV1 {
        let chunk_id = id::<CodeSearchChunkId>(chunk_id);
        let chunk_digest = content_digest(chunk_digest);
        let values = vec![value];
        let output_digest = tracedecay_semantic::projector::vector_output_digest(
            projection.projection_key(),
            &chunk_id,
            &chunk_digest,
            &values,
        )
        .expect("vector digest");
        ProjectedChunkVectorV1 {
            projection_key: projection.projection_key().clone(),
            source_generation: request.changes.to_generation.clone(),
            source_manifest_digest: request.changes.manifest_digest.clone(),
            chunk_id,
            chunk_digest,
            values,
            output_digest,
        }
    }

    fn prepared_fixture() -> PreparedVectorGenerationV1 {
        let embedding = embedding();
        let request = request(
            &[
                ("chunk.added", None, 'b'),
                ("chunk.updated", Some('f'), 'd'),
            ],
            &[("chunk.deleted", 'c')],
            &[("chunk.reused", 'e')],
        );
        let added = vector(&embedding, &request, "chunk.added", 'b', 0.25);
        let updated = vector(&embedding, &request, "chunk.updated", 'd', 0.5);
        let decisions = vec![
            ChunkProjectionDecisionV1 {
                chunk_id: added.chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(added.chunk_digest.clone()),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(added.output_digest.clone()),
            },
            ChunkProjectionDecisionV1 {
                chunk_id: id("chunk.deleted"),
                prior_chunk_digest: Some(content_digest('c')),
                current_chunk_digest: None,
                operation: ProjectionOperationV1::Deleted,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: None,
            },
            ChunkProjectionDecisionV1 {
                chunk_id: updated.chunk_id.clone(),
                prior_chunk_digest: Some(content_digest('f')),
                current_chunk_digest: Some(updated.chunk_digest.clone()),
                operation: ProjectionOperationV1::Updated,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(updated.output_digest.clone()),
            },
            ChunkProjectionDecisionV1 {
                chunk_id: id("chunk.reused"),
                prior_chunk_digest: Some(content_digest('e')),
                current_chunk_digest: Some(content_digest('e')),
                operation: ProjectionOperationV1::Reused,
                outcome: ProjectionOutcomeV1::Reused,
                output_digest: None,
            },
        ];
        let receipt = build_batch_receipt(&request, &decisions).expect("fixture receipt");
        PreparedVectorGenerationV1 {
            embedding_key: embedding,
            request,
            receipt,
            vectors: vec![added, updated],
            tombstones: vec![VectorTombstoneV1 {
                chunk_id: id("chunk.deleted"),
                prior_chunk_digest: content_digest('c'),
            }],
        }
    }

    fn reference_page(
        prepared: &PreparedVectorGenerationV1,
        page_request: ProjectionBatchRequestV1,
    ) -> PreparedVectorGenerationV1 {
        let mut vectors = Vec::new();
        let mut tombstones = Vec::new();
        let mut decisions = Vec::new();
        for change in &page_request.changes.added_or_changed {
            let mut vector = prepared
                .vectors
                .iter()
                .find(|vector| vector.chunk_id == change.chunk_id)
                .cloned()
                .expect("reference added vector");
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
                .expect("reference tombstone");
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
            } else {
                decisions.push(ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                });
            }
        }
        let receipt = build_batch_receipt(&page_request, &decisions).expect("reference receipt");
        PreparedVectorGenerationV1 {
            embedding_key: prepared.embedding_key.clone(),
            request: page_request,
            receipt,
            vectors,
            tombstones,
        }
    }

    #[test]
    fn indexed_page_reconstruction_is_byte_equal_to_the_reference_algorithm() {
        let prepared = prepared_fixture();
        let expected = reference_page(&prepared, prepared.request.clone());
        let mut index = EvaluationPreparedPageIndexV1::new(prepared.clone())
            .expect("duplicate-free prepared index");
        let actual = evaluation_prepared_page(&mut index, prepared.request.clone())
            .expect("indexed page reconstruction");
        index.finish().expect("page consumes every prepared row");

        assert_eq!(
            serde_json::to_vec(&actual).expect("actual canonical bytes"),
            serde_json::to_vec(&expected).expect("expected canonical bytes")
        );
    }

    #[test]
    fn indexed_page_reconstruction_rejects_duplicates_and_unrequested_rows() {
        let mut duplicate = prepared_fixture();
        duplicate.vectors.push(duplicate.vectors[0].clone());
        assert!(EvaluationPreparedPageIndexV1::new(duplicate).is_err());

        let extra = prepared_fixture();
        let page_request = request(&[], &[("chunk.deleted", 'c')], &[]);
        let mut index =
            EvaluationPreparedPageIndexV1::new(extra).expect("duplicate-free extra fixture");
        let _ = evaluation_prepared_page(&mut index, page_request)
            .expect("page with deliberately unrequested vectors");
        assert!(index.finish().is_err());
    }
}
