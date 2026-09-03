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
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, VectorTombstoneV1, split_projection_request,
};
use tracedecay_semantic_contracts::SemanticRuntimeScheduleFailureV1;

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
    chunks: &[Arc<CodeSearchChunkV1>],
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
///
/// The prepared corpus is borrowed and each page is reconstructed only when
/// its commit runs, then dropped: the additional live float set is one page,
/// never a second copy of the corpus (the whole-corpus materialization this
/// pages over is the evaluation journey's own retained projection).
#[hotpath::measure(label = "semantic.evaluation.commit_paged", future = true)]
pub(super) async fn commit_evaluation_prepared_generation(
    store: &GraphVectorGenerationStoreV1,
    build: &VectorGenerationBuildIdV1,
    prepared: &PreparedVectorGenerationV1,
    canonical_chunks: &[Arc<CodeSearchChunkV1>],
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
            .commit_batch(build, None, prepared.clone(), cancellation)
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        return Ok(());
    }
    let mut index = EvaluationPreparedPageIndexV1::new(prepared)?;
    for page in pages {
        let page = evaluation_prepared_page(&mut index, page.request)?;
        checkpoint = Some(
            store
                .commit_batch(build, checkpoint.as_ref(), page, Arc::clone(&cancellation))
                .await
                .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
        );
    }
    index.finish()?;
    Ok(())
}

/// Borrowed row index over one prepared corpus. Pages consume entries so a
/// row can serve exactly one page and [`Self::finish`] can prove nothing was
/// left unrequested; the corpus itself is never copied.
struct EvaluationPreparedPageIndexV1<'corpus> {
    embedding_key: &'corpus AdmittedEmbeddingProjectionKeyV1,
    vectors: BTreeMap<&'corpus CodeSearchChunkId, &'corpus ProjectedChunkVectorV1>,
    tombstones: BTreeMap<&'corpus CodeSearchChunkId, &'corpus VectorTombstoneV1>,
}

impl<'corpus> EvaluationPreparedPageIndexV1<'corpus> {
    fn new(
        prepared: &'corpus PreparedVectorGenerationV1,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let mut vectors = BTreeMap::new();
        for vector in &prepared.vectors {
            if vectors.insert(&vector.chunk_id, vector).is_some() {
                return Err(SemanticRuntimeScheduleFailureV1::projection(format!(
                    "evaluation paging received duplicate prepared vector {}",
                    vector.chunk_id
                )));
            }
        }
        let mut tombstones = BTreeMap::new();
        for tombstone in &prepared.tombstones {
            if tombstones.insert(&tombstone.chunk_id, tombstone).is_some() {
                return Err(SemanticRuntimeScheduleFailureV1::projection(format!(
                    "evaluation paging received duplicate prepared tombstone {}",
                    tombstone.chunk_id
                )));
            }
        }
        Ok(Self {
            embedding_key: &prepared.embedding_key,
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

/// Reconstruct one page-sized `PreparedVectorGenerationV1`, cloning only the
/// rows this page names out of the borrowed corpus.
#[hotpath::measure(label = "semantic.evaluation.page")]
fn evaluation_prepared_page(
    prepared: &mut EvaluationPreparedPageIndexV1<'_>,
    page_request: ProjectionBatchRequestV1,
) -> Result<PreparedVectorGenerationV1, SemanticRuntimeScheduleFailureV1> {
    let mut vectors = Vec::new();
    let mut tombstones = Vec::new();
    let mut decisions = Vec::new();
    for change in &page_request.changes.added_or_changed {
        let mut vector = prepared
            .vectors
            .remove(&change.chunk_id)
            .ok_or_else(|| {
                SemanticRuntimeScheduleFailureV1::projection(format!(
                    "evaluation page is missing prepared vector {}",
                    change.chunk_id
                ))
            })?
            .clone();
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
            })?
            .clone();
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
        if let Some(vector) = prepared.vectors.remove(&change.chunk_id) {
            let mut vector = vector.clone();
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
        BoundedSanitizedText, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
        CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, ContentDigest,
        EmbeddingDeviceClassV1, EmbeddingDocumentCompositionV1, EmbeddingMetricV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, FileOccurrenceId,
        LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId, PrivacyDomainId,
        ProjectionReplayReasonV1, SanitizerRevision, SensitivityDecision, SensitivityLevelV1,
        SourceSpan,
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
            document_composition: EmbeddingDocumentCompositionV1::SanitizedText,
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
        let mut index =
            EvaluationPreparedPageIndexV1::new(&prepared).expect("duplicate-free prepared index");
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
        assert!(EvaluationPreparedPageIndexV1::new(&duplicate).is_err());

        let extra = prepared_fixture();
        let page_request = request(&[], &[("chunk.deleted", 'c')], &[]);
        let mut index =
            EvaluationPreparedPageIndexV1::new(&extra).expect("duplicate-free extra fixture");
        let _ = evaluation_prepared_page(&mut index, page_request)
            .expect("page with deliberately unrequested vectors");
        assert!(index.finish().is_err());
    }

    fn canonical_chunk(
        chunk_id: &str,
        generation: &CodeGenerationId,
        digest: char,
        ordinal: u32,
    ) -> Arc<CodeSearchChunkV1> {
        let text = format!("canonical chunk body {ordinal}");
        Arc::new(CodeSearchChunkV1 {
            id: id(chunk_id),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation.clone(),
                file_occurrence_id: FileOccurrenceId::new(format!("{chunk_id}.rs"))
                    .expect("file fixture"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: u64::try_from(text.len()).expect("fixture span"),
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: content_digest(digest),
            language_descriptor_revision: LanguageDescriptorRevision::new("rust.v1")
                .expect("language fixture"),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("sanitizer fixture"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.v1").expect("policy fixture"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new(&text).expect("sanitized fixture"),
        })
    }

    /// Committing lazily reconstructed pages publishes byte-identical vector
    /// content to committing the whole prepared corpus in one batch: same
    /// generation identity, same rows. Only execution evidence (per-batch
    /// receipts and the checkpoint) legitimately differs. Tombstone and
    /// reused-lane page reconstruction is proven byte-exactly by
    /// `indexed_page_reconstruction_is_byte_equal_to_the_reference_algorithm`.
    #[test]
    fn paged_commit_publishes_identical_content_to_the_unpaged_commit() {
        use crate::store::vector_generations::{
            VectorGenerationPlanV1, VectorGenerationStateMachineV1,
        };

        let embedding = embedding();
        let added = (0..6)
            .map(|ordinal| format!("chunk.added.{ordinal:02}"))
            .collect::<Vec<_>>();
        let added_specs = added
            .iter()
            .map(|chunk_id| (chunk_id.as_str(), None, 'b'))
            .collect::<Vec<_>>();
        let request = request(&added_specs, &[], &[]);
        let canonical_chunks = added
            .iter()
            .enumerate()
            .map(|(ordinal, chunk_id)| {
                canonical_chunk(
                    chunk_id,
                    &request.changes.to_generation,
                    'b',
                    u32::try_from(ordinal).expect("fixture ordinal"),
                )
            })
            .collect::<Vec<_>>();
        let vectors = added
            .iter()
            .enumerate()
            .map(|(ordinal, chunk_id)| {
                vector(&embedding, &request, chunk_id, 'b', 0.125 + ordinal as f32)
            })
            .collect::<Vec<_>>();
        let decisions = vectors
            .iter()
            .map(|vector| ChunkProjectionDecisionV1 {
                chunk_id: vector.chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(vector.chunk_digest.clone()),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(vector.output_digest.clone()),
            })
            .collect::<Vec<_>>();
        let receipt = build_batch_receipt(&request, &decisions).expect("corpus receipt");
        let prepared = PreparedVectorGenerationV1 {
            embedding_key: embedding.clone(),
            request,
            receipt,
            vectors,
            tombstones: Vec::new(),
        };
        let plan = VectorGenerationPlanV1 {
            target_projection_key: embedding.projection_key().clone(),
            source_generation: prepared.request.changes.to_generation.clone(),
            source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
            expected_chunk_ids: prepared
                .vectors
                .iter()
                .map(|vector| vector.chunk_id.clone())
                .collect::<Vec<_>>()
                .into(),
            base_generation: None,
        };

        let mut unpaged = VectorGenerationStateMachineV1::new();
        let unpaged_build = unpaged
            .begin_generation(plan.clone())
            .expect("unpaged build");
        unpaged
            .commit_batch(&unpaged_build, None, prepared.clone())
            .expect("one whole-corpus commit");
        let unpaged_publication = unpaged
            .publish_generation(&unpaged_build)
            .expect("unpaged publication");

        // Two encoder groups per page (inference batch size 8 from the
        // fixture would keep everything on one page, so page by pairs).
        let pages = split_projection_request(&prepared.request, &canonical_chunks, 2, 2, 1 << 20)
            .expect("paged split");
        assert!(
            pages.len() > 1,
            "the fixture must actually split into multiple pages"
        );
        let mut index =
            EvaluationPreparedPageIndexV1::new(&prepared).expect("borrowed corpus index");
        let mut paged = VectorGenerationStateMachineV1::new();
        let paged_build = paged.begin_generation(plan).expect("paged build");
        let mut checkpoint = None;
        for page in pages {
            let page = evaluation_prepared_page(&mut index, page.request)
                .expect("lazily reconstructed page");
            checkpoint = Some(
                paged
                    .commit_batch(&paged_build, checkpoint.as_ref(), page)
                    .expect("paged commit"),
            );
        }
        index.finish().expect("every prepared row served one page");
        let paged_publication = paged
            .publish_generation(&paged_build)
            .expect("paged publication");

        assert_eq!(
            paged_publication.generation_id,
            unpaged_publication.generation_id
        );
        assert_eq!(
            paged_publication.manifest_digest,
            unpaged_publication.manifest_digest
        );
        let paged_generation = paged
            .generation(&paged_publication.generation_id)
            .expect("paged generation");
        let unpaged_generation = unpaged
            .generation(&unpaged_publication.generation_id)
            .expect("unpaged generation");
        assert_eq!(
            serde_json::to_vec(paged_generation.vectors()).expect("paged rows"),
            serde_json::to_vec(unpaged_generation.vectors()).expect("unpaged rows"),
            "paged and unpaged publications carry byte-identical vector rows"
        );
        assert_eq!(
            paged_generation.tombstone_digests(),
            unpaged_generation.tombstone_digests()
        );
        assert!(
            paged_generation.receipts().len() > unpaged_generation.receipts().len(),
            "execution evidence is per batch by design"
        );
    }
}
