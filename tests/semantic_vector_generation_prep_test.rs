#![allow(clippy::cloned_ref_to_slice_refs, clippy::drop_non_drop)] // test builders and explicit early drops
#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::sync::mpsc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay::code_index::projection::{expected_request_digest, verify_batch_receipt};
use tracedecay::store::vector_generation_test_support::{
    CanonicalChunkVectorEncoderV1, ProjectionRequestBatchV1, SemanticProjectionErrorV1,
    VectorGenerationIdV1, VectorGenerationPlanV1, VectorGenerationStateMachineV1,
    VectorGenerationStoreErrorV1, prepare_vector_generation, prepare_vector_generation_async,
    split_projection_request,
};
use tracedecay_domain::{
    BoundedSanitizedText, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
    CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId,
    CodeSearchChunkV1, EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1,
    EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1,
    FileOccurrenceId, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId,
    PrivacyDomainId, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1, SanitizerRevision,
    SemanticSearchIndexProfileV1, SensitivityDecision, SensitivityLevelV1, SourceSpan,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_string()).expect("canonical fixture identity")
}

fn digest(label: u8) -> ManifestDigest {
    id(&format!("sha256:{}", format!("{label:02x}").repeat(32)))
}

fn content_digest(bytes: &[u8]) -> tracedecay_domain::ContentDigest {
    let mut encoded = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string is infallible");
    }
    id(&encoded)
}

fn embedding_key() -> EmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest(1),
        tokenizer_digest: digest(2),
        config_digest: digest(3),
        query_instruction_digest: Some(digest(4)),
        document_instruction_digest: Some(digest(5)),
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        inference_batch_size: 8,
        inference_batch_bytes: 16 * 1024,
        runtime_backend: "fastembed-ort".to_string(),
        runtime_build_revision: "ort-test-rev-1".to_string(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 4,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_string(),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.project-a"),
        privacy_key_epoch: 7,
    }
}

fn admitted_key(
    key: &EmbeddingProjectionKeyV1,
) -> tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
    key.admit().expect("valid embedding projection admission")
}

fn chunk(generation: &str, name: &str, text: &str, ordinal: u32) -> CodeSearchChunkV1 {
    CodeSearchChunkV1 {
        id: id::<CodeSearchChunkId>(&format!("chunk.v1.{name}")),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: id::<CodeGenerationId>(generation),
            file_occurrence_id: id::<FileOccurrenceId>(&format!("file.v1.{name}")),
            symbol_occurrence_id: None,
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: text.len() as u64,
            },
            grain: CodeSearchChunkGrainV1::FileWindow,
            ordinal,
        },
        content_digest: content_digest(text.as_bytes()),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("descriptor.rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Internal,
            policy_revision: id::<PolicyRevisionId>("policy.v1"),
        },
        exact_terms: vec![],
        subtokens: vec![],
        sanitized_text: BoundedSanitizedText::new(text).expect("bounded fixture text"),
    }
}

fn chunk_in_file(
    generation: &str,
    file: &str,
    name: &str,
    text: &str,
    ordinal: u32,
) -> CodeSearchChunkV1 {
    let mut chunk = chunk(generation, name, text, ordinal);
    let start_byte = u64::from(ordinal).saturating_mul(128);
    chunk.anchor.file_occurrence_id = id::<FileOccurrenceId>(&format!("file.v1.{file}"));
    chunk.anchor.source_span = SourceSpan {
        start_byte,
        end_byte: start_byte.saturating_add(text.len() as u64),
    };
    chunk
}

fn interleaved_file_chunks(
    generation: &str,
    file: &str,
    content: &str,
    chunk_count: u32,
) -> Vec<CodeSearchChunkV1> {
    (0..chunk_count)
        .map(|index| {
            let text = format!("fn {content}_{index:02}() {{}}");
            chunk_in_file(
                generation,
                file,
                &format!("{index:02}-{file}"),
                &text,
                index,
            )
        })
        .collect()
}

fn change(
    chunk: &CodeSearchChunkV1,
    prior: Option<tracedecay_domain::ContentDigest>,
    current: Option<tracedecay_domain::ContentDigest>,
) -> ChangedCodeChunkV1 {
    ChangedCodeChunkV1 {
        chunk_id: chunk.id.clone(),
        prior_digest: prior,
        current_digest: current,
    }
}

fn changes(
    from_generation: Option<&str>,
    to_generation: &str,
    added_or_changed: Vec<ChangedCodeChunkV1>,
    deleted: Vec<ChangedCodeChunkV1>,
    reused: Vec<ChangedCodeChunkV1>,
) -> ChangedCodeChunkSetV1 {
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: from_generation.map(id::<CodeGenerationId>),
        to_generation: id::<CodeGenerationId>(to_generation),
        manifest_digest: digest(0),
        added_or_changed,
        deleted,
        reused,
    };
    changes.manifest_digest = changes.compute_digest().expect("changed set digest");
    changes.validate().expect("canonical changed set");
    changes
}

fn request(
    changes: ChangedCodeChunkSetV1,
    previous_projection_key: Option<ProjectionKeyV1>,
    target_projection_key: ProjectionKeyV1,
    replay_reason: ProjectionReplayReasonV1,
) -> ProjectionBatchRequestV1 {
    let mut request = ProjectionBatchRequestV1 {
        request_digest: digest(0),
        changes,
        previous_projection_key,
        target_projection_key,
        replay_reason,
    };
    request.request_digest = expected_request_digest(&request).expect("request digest");
    request
}

#[derive(Default)]
struct FakeEncoder {
    seen: Vec<CodeSearchChunkId>,
    batch_sizes: Vec<usize>,
    batches: Vec<Vec<CodeSearchChunkId>>,
    batch_shape_sensitive: bool,
    dimension_delta: isize,
    non_finite: bool,
}

impl CanonicalChunkVectorEncoderV1 for FakeEncoder {
    fn encode(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<Vec<f32>, String> {
        self.seen.push(chunk.id.clone());
        let dimensions = (key.dimensions as isize + self.dimension_delta) as usize;
        let seed = chunk
            .sanitized_text
            .as_str()
            .bytes()
            .fold(0u32, |sum, byte| sum.wrapping_add(u32::from(byte)));
        let mut values = (0..dimensions)
            .map(|index| (seed.wrapping_add(index as u32) % 101) as f32 / 101.0)
            .collect::<Vec<_>>();
        if self.non_finite {
            values[0] = f32::NAN;
        }
        Ok(values)
    }

    fn encode_batch(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<Vec<f32>>, String> {
        self.batch_sizes.push(chunks.len());
        self.batches
            .push(chunks.iter().map(|chunk| chunk.id.clone()).collect());
        let mut vectors = chunks
            .iter()
            .map(|chunk| self.encode(key, chunk))
            .collect::<Result<Vec<_>, _>>()?;
        if self.batch_shape_sensitive {
            for vector in &mut vectors {
                if let Some(first) = vector.first_mut() {
                    *first += chunks.len() as f32;
                }
            }
        }
        Ok(vectors)
    }
}

struct BlockingEncoder {
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl CanonicalChunkVectorEncoderV1 for BlockingEncoder {
    fn encode(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        _chunk: &CodeSearchChunkV1,
    ) -> Result<Vec<f32>, String> {
        self.started.send(()).map_err(|error| error.to_string())?;
        self.release.recv().map_err(|error| error.to_string())?;
        Ok(vec![0.25; key.dimensions as usize])
    }
}

fn publish_initial_generation() -> (
    VectorGenerationStateMachineV1,
    EmbeddingProjectionKeyV1,
    VectorGenerationIdV1,
) {
    let key = embedding_key();
    let projection_key = key.projection_key().expect("valid semantic key");
    let alpha = chunk("code-generation.1", "alpha", "fn alpha() -> u8 { 1 }", 0);
    let gone = chunk("code-generation.1", "gone", "fn gone() {}", 1);
    let stable = chunk("code-generation.1", "stable", "fn stable() {}", 2);
    let initial_changes = changes(
        None,
        "code-generation.1",
        vec![
            change(&alpha, None, Some(alpha.content_digest.clone())),
            change(&gone, None, Some(gone.content_digest.clone())),
            change(&stable, None, Some(stable.content_digest.clone())),
        ],
        vec![],
        vec![],
    );
    let initial_request = request(
        initial_changes,
        None,
        projection_key.clone(),
        ProjectionReplayReasonV1::InitialProjection,
    );
    let mut encoder = FakeEncoder::default();
    let prepared = prepare_vector_generation(
        &admitted_key(&key),
        initial_request,
        &[alpha.clone(), gone.clone(), stable.clone()],
        &mut encoder,
    )
    .expect("initial fake projection");
    assert_eq!(encoder.seen.len(), 3);
    assert_eq!(encoder.batch_sizes, [3]);

    let mut store = VectorGenerationStateMachineV1::new();
    let build_id = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: id("code-generation.1"),
            source_manifest_digest: prepared.receipt.source_manifest_digest.clone(),
            expected_chunk_ids: vec![alpha.id, gone.id, stable.id].into(),
            base_generation: None,
        })
        .expect("initial build");
    let checkpoint = store
        .commit_batch(&build_id, None, prepared.clone())
        .expect("initial batch");
    let duplicate_checkpoint = store
        .commit_batch(&build_id, None, prepared)
        .expect("lost-ack duplicate is an idempotent no-op");
    assert_eq!(duplicate_checkpoint, checkpoint);
    let publication = store
        .publish_generation(&build_id)
        .expect("initial publication");
    let published = store
        .generation(&publication.generation_id)
        .expect("initial generation");
    assert_eq!(published.generation_id(), &publication.generation_id);
    assert_eq!(
        publication.generation_id.as_digest(),
        &publication.manifest_digest
    );
    assert_eq!(published.projection_key(), &projection_key);
    assert_eq!(
        published.source_generation(),
        &id::<CodeGenerationId>("code-generation.1")
    );
    assert_eq!(
        published.source_manifest_digest(),
        &published.checkpoint().source_manifest_digest
    );
    assert_eq!(published.manifest_digest(), &publication.manifest_digest);
    (store, key, publication.generation_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn indexing_and_cancellation_leave_only_the_compatible_prior_generation_queryable() {
    let (mut store, key, base_generation) = publish_initial_generation();
    let admitted = admitted_key(&key);
    let projection_key = admitted.projection_key().clone();
    let prior = store
        .generation(&base_generation)
        .expect("published prior generation");
    let prior_source_generation = prior.source_generation().clone();
    let prior_source_manifest = prior.source_manifest_digest().clone();
    let alpha = chunk("code-generation.2", "alpha", "fn alpha() -> u8 { 2 }", 0);
    let projection_request = request(
        changes(
            Some("code-generation.1"),
            "code-generation.2",
            vec![change(
                &alpha,
                Some(content_digest(b"fn alpha() -> u8 { 1 }")),
                Some(alpha.content_digest.clone()),
            )],
            vec![],
            vec![],
        ),
        Some(projection_key.clone()),
        projection_key.clone(),
        ProjectionReplayReasonV1::SourceEdit,
    );
    let next_source_manifest = projection_request.changes.manifest_digest.clone();
    let build_id = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key,
            source_generation: id("code-generation.2"),
            source_manifest_digest: next_source_manifest.clone(),
            expected_chunk_ids: vec![alpha.id.clone()].into(),
            base_generation: Some(base_generation.clone()),
        })
        .expect("staged replacement");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let projection = tokio::spawn(prepare_vector_generation_async(
        admitted.clone(),
        projection_request,
        vec![alpha],
        BlockingEncoder {
            started: started_tx,
            release: release_rx,
        },
    ));

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("background projection started");
    assert!(
        store
            .generation(&base_generation)
            .filter(|generation| {
                generation.embedding_key() == &admitted
                    && generation.source_generation() == &prior_source_generation
                    && generation.source_manifest_digest() == &prior_source_manifest
            })
            .is_some(),
        "an exactly compatible prior snapshot remains queryable while indexing"
    );
    assert!(
        store
            .generation(&base_generation)
            .filter(|generation| {
                generation.source_generation() == &id("code-generation.2")
                    && generation.source_manifest_digest() == &next_source_manifest
            })
            .is_none(),
        "the partial replacement is omitted instead of exposing stale semantic rows"
    );

    release_tx.send(()).expect("release projector");
    let prepared = projection
        .await
        .expect("projection task joined")
        .expect("projection completed");
    store
        .commit_batch(&build_id, None, prepared)
        .expect("checkpoint completed batch");
    assert!(store.cancel_generation(&build_id));
    assert_eq!(
        store.publish_generation(&build_id),
        Err(VectorGenerationStoreErrorV1::UnknownBuild)
    );
}

#[test]
fn semantic_projection_key_is_complete_deterministic_and_maps_to_projection_key() {
    let key = embedding_key();
    key.validate().expect("valid key");
    let first = key.canonical_digest().expect("key digest");
    let second = key.canonical_digest().expect("stable replay");
    assert_eq!(first, second);

    let generic = key.projection_key().expect("generic projection key");
    assert_eq!(generic.kind, ProjectionKindV1::Embedding);
    assert_eq!(generic.profile_digest, first);

    let mut changed = key.clone();
    changed.runtime_build_revision = "ort-test-rev-2".to_string();
    assert_ne!(
        changed.canonical_digest().unwrap(),
        key.canonical_digest().unwrap()
    );
    changed = key.clone();
    changed.model_artifact_digest = digest(b'9');
    assert_ne!(
        changed.canonical_digest().unwrap(),
        key.canonical_digest().unwrap(),
        "model identity changes must force a new vector projection"
    );
    changed = key.clone();
    changed.privacy_key_epoch += 1;
    assert_ne!(
        changed.canonical_digest().unwrap(),
        key.canonical_digest().unwrap()
    );
    changed = key.clone();
    changed.chunker_revision = id("chunker.v2");
    assert_ne!(
        changed.canonical_digest().unwrap(),
        key.canonical_digest().unwrap()
    );
}

#[test]
fn semantic_search_index_identity_changes_without_reprojecting_vectors() {
    let projection = embedding_key();
    let projection_digest = projection.canonical_digest().unwrap();
    let exact = SemanticSearchIndexProfileV1::exact_flat_v1().unwrap();
    let exact_key = exact.index_key().unwrap();

    let mut changed = exact;
    changed.implementation_revision = "semantic.exact-flat.v2".to_owned();
    let changed_key = changed.index_key().unwrap();

    assert_ne!(exact_key, changed_key);
    assert_eq!(
        projection.canonical_digest().unwrap(),
        projection_digest,
        "search-index-only changes must not alter vector projection identity"
    );
}

#[test]
fn admitted_projection_key_is_the_projection_and_privacy_authority() {
    let key = embedding_key();
    let admitted = key.admit().expect("valid projection admission");

    assert_eq!(admitted.embedding_key(), &key);
    assert_eq!(
        admitted.projection_key(),
        &key.projection_key().expect("generic projection key")
    );
    assert_eq!(admitted.privacy_domain(), &key.privacy_domain);
    assert_eq!(admitted.privacy_key_epoch(), key.privacy_key_epoch);

    let mut invalid = key;
    invalid.dimensions = 0;
    assert!(invalid.admit().is_err(), "invalid keys cannot be admitted");
}

#[test]
fn fake_projection_uses_canonical_chunks_and_projection_receipts() {
    let (mut store, key, base_generation) = publish_initial_generation();
    let projection_key = key.projection_key().unwrap();

    let alpha_old = chunk("code-generation.1", "alpha", "fn alpha() -> u8 { 1 }", 0);
    let gone_old = chunk("code-generation.1", "gone", "fn gone() {}", 1);
    let stable_old = chunk("code-generation.1", "stable", "fn stable() {}", 2);
    let alpha = chunk("code-generation.2", "alpha", "fn alpha() -> u8 { 2 }", 0);
    let added = chunk("code-generation.2", "new", "fn newly_added() {}", 1);
    let stable = chunk("code-generation.2", "stable", "fn stable() {}", 2);
    let changed = changes(
        Some("code-generation.1"),
        "code-generation.2",
        vec![
            change(
                &alpha,
                Some(alpha_old.content_digest),
                Some(alpha.content_digest.clone()),
            ),
            change(&added, None, Some(added.content_digest.clone())),
        ],
        vec![change(
            &gone_old,
            Some(gone_old.content_digest.clone()),
            None,
        )],
        vec![change(
            &stable,
            Some(stable_old.content_digest.clone()),
            Some(stable.content_digest.clone()),
        )],
    );
    let projection_request = request(
        changed,
        Some(projection_key.clone()),
        projection_key.clone(),
        ProjectionReplayReasonV1::SourceEdit,
    );
    let mut encoder = FakeEncoder::default();
    let prepared = prepare_vector_generation(
        &admitted_key(&key),
        projection_request,
        &[alpha.clone(), added.clone()],
        &mut encoder,
    )
    .expect("changed fake projection");

    assert_eq!(encoder.seen, vec![alpha.id.clone(), added.id.clone()]);
    assert_eq!(prepared.vectors.len(), 2);
    assert_eq!(prepared.tombstones.len(), 1);
    assert_eq!(prepared.receipt.receipts.len(), 4);
    verify_batch_receipt(&prepared.request, &prepared.receipt).expect("projection receipt");

    let build_id = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key,
            source_generation: id("code-generation.2"),
            source_manifest_digest: prepared.receipt.source_manifest_digest.clone(),
            expected_chunk_ids: vec![alpha.id.clone(), added.id.clone(), stable.id.clone()].into(),
            base_generation: Some(base_generation.clone()),
        })
        .expect("changed build");
    let checkpoint = store
        .commit_batch(&build_id, None, prepared)
        .expect("changed batch");
    assert_eq!(checkpoint.completed_batches, 1);
    let publication = store
        .publish_generation(&build_id)
        .expect("changed publication");
    let published = store
        .generation(&publication.generation_id)
        .expect("published generation");
    assert_eq!(published.vectors().len(), 3);
    assert!(published.vectors().contains_key(&alpha.id));
    assert!(published.vectors().contains_key(&added.id));
    assert!(published.vectors().contains_key(&stable.id));
    assert_eq!(published.tombstones(), &[gone_old.id]);
    assert_eq!(published.receipts().len(), 1);
}

#[test]
fn committed_checkpoint_remains_staged_until_immutable_publication() {
    let (mut store, key, base_generation) = publish_initial_generation();
    let projection_key = key.projection_key().unwrap();
    let alpha = chunk("code-generation.2", "alpha", "fn alpha() -> u8 { 2 }", 0);
    let prepared = prepare_vector_generation(
        &admitted_key(&key),
        request(
            changes(
                Some("code-generation.1"),
                "code-generation.2",
                vec![change(
                    &alpha,
                    Some(content_digest(b"fn alpha() -> u8 { 1 }")),
                    Some(alpha.content_digest.clone()),
                )],
                vec![],
                vec![],
            ),
            Some(projection_key.clone()),
            projection_key.clone(),
            ProjectionReplayReasonV1::SourceEdit,
        ),
        std::slice::from_ref(&alpha),
        &mut FakeEncoder::default(),
    )
    .unwrap();
    let build_id = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key,
            source_generation: id("code-generation.2"),
            source_manifest_digest: prepared.receipt.source_manifest_digest.clone(),
            expected_chunk_ids: vec![alpha.id].into(),
            base_generation: None,
        })
        .unwrap();
    store.commit_batch(&build_id, None, prepared).unwrap();

    let staged_checkpoint = store
        .staged_checkpoint(&build_id)
        .cloned()
        .expect("durable staged checkpoint");
    assert!(store.generation(&base_generation).is_some());

    let publication = store
        .publish_generation(&build_id)
        .expect("publishes immutable generation");
    assert_eq!(publication.checkpoint, staged_checkpoint);
    assert_eq!(
        store
            .generation(&publication.generation_id)
            .map(|generation| generation.checkpoint()),
        Some(&publication.checkpoint)
    );
}

#[test]
fn unchanged_generation_reuses_vectors_without_fake_inference() {
    let (mut store, key, base_generation) = publish_initial_generation();
    let projection_key = key.projection_key().unwrap();
    let alpha = chunk("code-generation.2", "alpha", "fn alpha() -> u8 { 1 }", 0);
    let gone = chunk("code-generation.2", "gone", "fn gone() {}", 1);
    let stable = chunk("code-generation.2", "stable", "fn stable() {}", 2);
    let no_op = changes(
        Some("code-generation.1"),
        "code-generation.2",
        vec![],
        vec![],
        vec![
            change(
                &alpha,
                Some(alpha.content_digest.clone()),
                Some(alpha.content_digest.clone()),
            ),
            change(
                &gone,
                Some(gone.content_digest.clone()),
                Some(gone.content_digest.clone()),
            ),
            change(
                &stable,
                Some(stable.content_digest.clone()),
                Some(stable.content_digest.clone()),
            ),
        ],
    );
    let mut encoder = FakeEncoder::default();
    let prepared = prepare_vector_generation(
        &admitted_key(&key),
        request(
            no_op,
            Some(projection_key.clone()),
            projection_key.clone(),
            ProjectionReplayReasonV1::VerificationReplay,
        ),
        &[],
        &mut encoder,
    )
    .expect("no-op replay");
    assert!(encoder.seen.is_empty());
    assert!(prepared.vectors.is_empty());
    assert!(prepared.tombstones.is_empty());

    let build_id = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key,
            source_generation: id("code-generation.2"),
            source_manifest_digest: prepared.receipt.source_manifest_digest.clone(),
            expected_chunk_ids: vec![alpha.id, gone.id, stable.id].into(),
            base_generation: Some(base_generation.clone()),
        })
        .unwrap();
    store.commit_batch(&build_id, None, prepared).unwrap();
    let published = store.publish_generation(&build_id).unwrap();
    let incremental = store.generation(&published.generation_id).unwrap();
    let base = store.generation(&base_generation).unwrap();
    assert_eq!(incremental.vectors().len(), 3);
    for (chunk_id, vector) in incremental.vectors() {
        let prior = base
            .vectors()
            .get(chunk_id)
            .expect("reused chunk must exist on the base generation");
        assert_eq!(vector.values, prior.values);
        assert_eq!(vector.chunk_digest, prior.chunk_digest);
    }
}

#[test]
fn invalid_fake_vectors_and_key_only_reuse_fail_closed() {
    let key = embedding_key();
    let projection_key = key.projection_key().unwrap();
    let alpha = chunk("code-generation.1", "alpha", "fn alpha() {}", 0);
    let initial = request(
        changes(
            None,
            "code-generation.1",
            vec![change(&alpha, None, Some(alpha.content_digest.clone()))],
            vec![],
            vec![],
        ),
        None,
        projection_key.clone(),
        ProjectionReplayReasonV1::InitialProjection,
    );

    let mut wrong_dimensions = FakeEncoder {
        dimension_delta: -1,
        ..FakeEncoder::default()
    };
    assert!(matches!(
        prepare_vector_generation(
            &admitted_key(&key),
            initial.clone(),
            std::slice::from_ref(&alpha),
            &mut wrong_dimensions
        ),
        Err(SemanticProjectionErrorV1::VectorDimensionMismatch { .. })
    ));
    let mut non_finite = FakeEncoder {
        non_finite: true,
        ..FakeEncoder::default()
    };
    assert!(matches!(
        prepare_vector_generation(
            &admitted_key(&key),
            initial,
            std::slice::from_ref(&alpha),
            &mut non_finite
        ),
        Err(SemanticProjectionErrorV1::NonFiniteVector { .. })
    ));

    let mut replacement_key = key.clone();
    replacement_key.model_artifact_digest = digest(b'9');
    let replacement_projection_key = replacement_key.projection_key().unwrap();
    let key_only_replay = request(
        changes(
            Some("code-generation.1"),
            "code-generation.2",
            vec![],
            vec![],
            vec![change(
                &alpha,
                Some(alpha.content_digest.clone()),
                Some(alpha.content_digest.clone()),
            )],
        ),
        Some(projection_key),
        replacement_projection_key,
        ProjectionReplayReasonV1::ProjectionProfileChange,
    );
    let mut encoder = FakeEncoder::default();
    assert_eq!(
        prepare_vector_generation(
            &admitted_key(&replacement_key),
            key_only_replay.clone(),
            &[],
            &mut encoder
        ),
        Err(SemanticProjectionErrorV1::KeyReplayRequiresExplicitEmbeds)
    );
    assert!(encoder.seen.is_empty());

    let alpha_v2 = chunk("code-generation.2", "alpha", "fn alpha() {}", 0);
    assert_eq!(alpha.content_digest, alpha_v2.content_digest);
    let explicit_model_replay = request(
        changes(
            Some("code-generation.1"),
            "code-generation.2",
            vec![],
            vec![],
            vec![change(
                &alpha_v2,
                Some(alpha.content_digest.clone()),
                Some(alpha_v2.content_digest.clone()),
            )],
        ),
        key_only_replay.previous_projection_key,
        key_only_replay.target_projection_key,
        ProjectionReplayReasonV1::ProjectionProfileChange,
    );
    let prepared = prepare_vector_generation(
        &admitted_key(&replacement_key),
        explicit_model_replay,
        std::slice::from_ref(&alpha_v2),
        &mut encoder,
    )
    .expect("model-key replay explicitly projects every retained chunk");
    assert_eq!(encoder.seen, vec![alpha_v2.id.clone()]);
    assert_eq!(prepared.vectors.len(), 1);
    assert_eq!(prepared.receipt.reused_count, 0);
    assert_eq!(
        prepared.receipt.receipts[0].operation,
        ProjectionOperationV1::Updated
    );
    assert_eq!(
        prepared.receipt.receipts[0].outcome,
        ProjectionOutcomeV1::Applied
    );
}

#[test]
fn duplicate_vector_rows_fail_without_advancing_the_checkpoint() {
    let key = embedding_key();
    let projection_key = key.projection_key().unwrap();
    let alpha = chunk("code-generation.1", "alpha", "fn alpha() {}", 0);
    let prepared = prepare_vector_generation(
        &admitted_key(&key),
        request(
            changes(
                None,
                "code-generation.1",
                vec![change(&alpha, None, Some(alpha.content_digest.clone()))],
                vec![],
                vec![],
            ),
            None,
            projection_key.clone(),
            ProjectionReplayReasonV1::InitialProjection,
        ),
        std::slice::from_ref(&alpha),
        &mut FakeEncoder::default(),
    )
    .unwrap();
    let mut store = VectorGenerationStateMachineV1::new();
    let build_id = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key,
            source_generation: id("code-generation.1"),
            source_manifest_digest: prepared.receipt.source_manifest_digest.clone(),
            expected_chunk_ids: vec![alpha.id].into(),
            base_generation: None,
        })
        .unwrap();

    let mut duplicated = prepared.clone();
    duplicated.vectors.push(duplicated.vectors[0].clone());
    assert_eq!(
        store.commit_batch(&build_id, None, duplicated),
        Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch)
    );
    let checkpoint = store
        .commit_batch(&build_id, None, prepared)
        .expect("failed handoff left the prior checkpoint intact");
    assert_eq!(checkpoint.completed_batches, 1);
}

#[test]
fn one_batch_and_multi_batch_publications_have_equal_generation_identity() {
    let key = embedding_key();
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let alpha = chunk("code-generation.1", "alpha", "fn alpha() {}", 0);
    let beta = chunk("code-generation.1", "beta", "fn beta() {}", 1);
    let source_manifest_digest = digest(42);
    let plan = VectorGenerationPlanV1 {
        target_projection_key: projection_key.clone(),
        source_generation: id("code-generation.1"),
        source_manifest_digest,
        expected_chunk_ids: vec![alpha.id.clone(), beta.id.clone()].into(),
        base_generation: None,
    };

    let single_prepared = prepare_vector_generation(
        &admitted,
        request(
            changes(
                None,
                "code-generation.1",
                vec![
                    change(&alpha, None, Some(alpha.content_digest.clone())),
                    change(&beta, None, Some(beta.content_digest.clone())),
                ],
                vec![],
                vec![],
            ),
            None,
            projection_key.clone(),
            ProjectionReplayReasonV1::InitialProjection,
        ),
        &[alpha.clone(), beta.clone()],
        &mut FakeEncoder::default(),
    )
    .expect("single projection batch");
    let mut single_store = VectorGenerationStateMachineV1::new();
    let single_build = single_store
        .begin_generation(plan.clone())
        .expect("single build");
    single_store
        .commit_batch(&single_build, None, single_prepared)
        .expect("single batch commit");
    let single = single_store
        .publish_generation(&single_build)
        .expect("single batch publication");

    let alpha_prepared = prepare_vector_generation(
        &admitted,
        request(
            changes(
                None,
                "code-generation.1",
                vec![change(&alpha, None, Some(alpha.content_digest.clone()))],
                vec![],
                vec![],
            ),
            None,
            projection_key.clone(),
            ProjectionReplayReasonV1::InitialProjection,
        ),
        std::slice::from_ref(&alpha),
        &mut FakeEncoder::default(),
    )
    .expect("first projection batch");
    let beta_prepared = prepare_vector_generation(
        &admitted,
        request(
            changes(
                None,
                "code-generation.1",
                vec![change(&beta, None, Some(beta.content_digest.clone()))],
                vec![],
                vec![],
            ),
            None,
            projection_key,
            ProjectionReplayReasonV1::InitialProjection,
        ),
        std::slice::from_ref(&beta),
        &mut FakeEncoder::default(),
    )
    .expect("second projection batch");
    let mut multi_store = VectorGenerationStateMachineV1::new();
    let multi_build = multi_store.begin_generation(plan).expect("multi build");
    let checkpoint = multi_store
        .commit_batch(&multi_build, None, alpha_prepared)
        .expect("first batch commit");
    assert_eq!(
        multi_store.publish_generation(&multi_build),
        Err(VectorGenerationStoreErrorV1::IncompleteGeneration),
        "a partial batch checkpoint must never become readable"
    );
    multi_store
        .commit_batch(&multi_build, Some(&checkpoint), beta_prepared)
        .expect("second batch commit");
    let multi = multi_store
        .publish_generation(&multi_build)
        .expect("multi batch publication");

    assert_eq!(single.generation_id, multi.generation_id);
    assert_eq!(single.manifest_digest, multi.manifest_digest);
    assert_ne!(single.checkpoint, multi.checkpoint);
    assert_eq!(
        single_store
            .generation(&single.generation_id)
            .expect("single generation")
            .receipts()
            .len(),
        1
    );
    assert_eq!(
        multi_store
            .generation(&multi.generation_id)
            .expect("multi generation")
            .receipts()
            .len(),
        2
    );
}

/// A corpus large enough to span several encoder groups and several commits.
fn split_identity_corpus() -> Vec<CodeSearchChunkV1> {
    (0..40)
        .map(|index| {
            chunk(
                "code-generation.1",
                &format!("split-{index:03}"),
                &format!("fn split_{index:03}() {{}}"),
                index,
            )
        })
        .collect()
}

fn whole_corpus_request(
    corpus: &[CodeSearchChunkV1],
    projection_key: &ProjectionKeyV1,
) -> ProjectionBatchRequestV1 {
    let mut added = corpus
        .iter()
        .map(|chunk| change(chunk, None, Some(chunk.content_digest.clone())))
        .collect::<Vec<_>>();
    added.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    request(
        changes(None, "code-generation.1", added, vec![], vec![]),
        None,
        projection_key.clone(),
        ProjectionReplayReasonV1::InitialProjection,
    )
}

fn publish_in_batches(
    admitted: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    plan: VectorGenerationPlanV1,
    batches: Vec<ProjectionRequestBatchV1>,
) -> (
    VectorGenerationStateMachineV1,
    tracedecay::store::vector_generation_test_support::VectorGenerationPublicationV1,
) {
    let mut store = VectorGenerationStateMachineV1::new();
    let build = store.begin_generation(plan).expect("staged build");
    let mut checkpoint = None;
    for batch in batches {
        let prepared = prepare_vector_generation(
            admitted,
            batch.request,
            &batch.canonical_chunks,
            &mut FakeEncoder::default(),
        )
        .expect("projection batch");
        checkpoint = Some(
            store
                .commit_batch(&build, checkpoint.as_ref(), prepared)
                .expect("batch commit"),
        );
    }
    let publication = store.publish_generation(&build).expect("publication");
    (store, publication)
}

/// The A/B digest-equality probe for incremental commits.
///
/// Production no longer commits a whole corpus in one shot: it splits the
/// request with `split_projection_request` and commits each batch as it
/// completes. That is only admissible if splitting moves no identity, so this
/// runs the *same* corpus both ways and compares what identity is built from —
/// the vector floats, every `output_digest`, and the generation manifest
/// digest — rather than comparing the encodings that carry them.
#[test]
fn splitting_a_run_into_commits_preserves_every_generation_digest() {
    let key = embedding_key();
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let corpus = split_identity_corpus();
    let whole = whole_corpus_request(&corpus, &projection_key);
    let mut expected_chunk_ids = corpus
        .iter()
        .map(|chunk| chunk.id.clone())
        .collect::<Vec<_>>();
    expected_chunk_ids.sort();
    let plan = VectorGenerationPlanV1 {
        target_projection_key: projection_key,
        source_generation: id("code-generation.1"),
        source_manifest_digest: whole.changes.manifest_digest.clone(),
        expected_chunk_ids: expected_chunk_ids.into(),
        base_generation: None,
    };

    let unsplit = split_projection_request(
        &whole,
        &corpus,
        4_096,
        8,
        key.inference_batch_bytes as usize,
    )
    .expect("unsplit request");
    assert_eq!(
        unsplit.len(),
        1,
        "a corpus inside one batch window is not split at all"
    );
    let (single_store, single) = publish_in_batches(&admitted, plan.clone(), unsplit);

    // 16 embeds per batch is two encoder groups, so boundaries stay aligned.
    let split =
        split_projection_request(&whole, &corpus, 16, 8, key.inference_batch_bytes as usize)
            .expect("split request");
    assert_eq!(split.len(), 3, "40 changes split into windows of 16");
    assert!(
        split
            .iter()
            .all(|batch| !batch.request.changes.added_or_changed.is_empty()),
        "every batch carries embeds of its own"
    );
    let (split_store, multi) = publish_in_batches(&admitted, plan, split);

    assert_eq!(
        single.generation_id, multi.generation_id,
        "splitting a run must not move the generation identity"
    );
    assert_eq!(single.manifest_digest, multi.manifest_digest);

    let single_generation = single_store
        .generation(&single.generation_id)
        .expect("single generation");
    let split_generation = split_store
        .generation(&multi.generation_id)
        .expect("split generation");
    assert_eq!(
        single_generation.vectors().len(),
        corpus.len(),
        "the split run publishes the whole corpus"
    );
    for (chunk_id, expected) in single_generation.vectors() {
        let actual = split_generation
            .vectors()
            .get(chunk_id)
            .expect("split vector");
        assert_eq!(
            expected.values, actual.values,
            "vector bytes for {chunk_id} must be identical across batch sizes"
        );
        assert_eq!(
            expected.output_digest, actual.output_digest,
            "output digest for {chunk_id} must be identical across batch sizes"
        );
        assert_eq!(
            expected.source_manifest_digest,
            actual.source_manifest_digest
        );
    }

    // Execution lineage is the only thing that legitimately differs.
    assert_eq!(single_generation.receipts().len(), 1);
    assert_eq!(split_generation.receipts().len(), 3);
    assert_ne!(single.checkpoint, multi.checkpoint);
}

/// A projection-profile change runs added and reembedded-reuse chunks through
/// separate encoder passes. Pagination must preserve those lane boundaries:
/// co-filling an added page with reuse changes the reembedded group shape and
/// can therefore change native vector bytes.
#[test]
fn profile_change_paging_preserves_reembedded_reuse_encoder_groups_and_vectors() {
    let previous_key = embedding_key();
    let previous_projection_key = previous_key.projection_key().expect("previous key");
    let mut replacement_key = previous_key.clone();
    replacement_key.model_artifact_digest = digest(b'9');
    let replacement_projection_key = replacement_key.projection_key().expect("replacement key");
    let admitted = admitted_key(&replacement_key);
    let added = chunk(
        "code-generation.2",
        "reprofile-added",
        "fn reprofile_added() {}",
        0,
    );
    let mut reused_chunks = (0..40)
        .map(|index| {
            chunk(
                "code-generation.2",
                &format!("reprofile-reused-{index:02}"),
                &format!("fn reprofile_reused_{index:02}() {{}}"),
                index + 1,
            )
        })
        .collect::<Vec<_>>();
    reused_chunks.sort_by(|left, right| left.id.cmp(&right.id));
    let reprofile = request(
        changes(
            Some("code-generation.1"),
            "code-generation.2",
            vec![change(&added, None, Some(added.content_digest.clone()))],
            vec![],
            reused_chunks
                .iter()
                .map(|chunk| {
                    change(
                        chunk,
                        Some(chunk.content_digest.clone()),
                        Some(chunk.content_digest.clone()),
                    )
                })
                .collect(),
        ),
        Some(previous_projection_key),
        replacement_projection_key,
        ProjectionReplayReasonV1::ProjectionProfileChange,
    );
    let mut canonical_chunks = vec![added.clone()];
    canonical_chunks.extend(reused_chunks.clone());

    let unsplit = split_projection_request(
        &reprofile,
        &canonical_chunks,
        4_096,
        8,
        replacement_key.inference_batch_bytes as usize,
    )
    .expect("whole profile change");
    assert_eq!(unsplit.len(), 1);
    let mut unsplit_encoder = FakeEncoder::default();
    let unsplit_prepared = prepare_vector_generation(
        &admitted,
        unsplit[0].request.clone(),
        &unsplit[0].canonical_chunks,
        &mut unsplit_encoder,
    )
    .expect("whole profile change projection");

    let split = split_projection_request(
        &reprofile,
        &canonical_chunks,
        16,
        8,
        replacement_key.inference_batch_bytes as usize,
    )
    .expect("split profile change");
    assert_eq!(
        split.len(),
        4,
        "added and reembedded reuse page independently"
    );
    assert_eq!(split[0].request.changes.added_or_changed.len(), 1);
    assert!(split[0].request.changes.reused.is_empty());
    assert!(
        split[1..]
            .iter()
            .all(|page| page.request.changes.added_or_changed.is_empty())
    );
    assert_eq!(
        split[1..]
            .iter()
            .map(|page| page.request.changes.reused.len())
            .collect::<Vec<_>>(),
        vec![16, 16, 8]
    );

    let mut split_encoder = FakeEncoder::default();
    let mut split_vectors = Vec::new();
    for batch in split {
        let prepared = prepare_vector_generation(
            &admitted,
            batch.request,
            &batch.canonical_chunks,
            &mut split_encoder,
        )
        .expect("split profile change projection");
        split_vectors.extend(prepared.vectors);
    }

    assert_eq!(split_encoder.batches, unsplit_encoder.batches);
    assert_eq!(split_encoder.seen, unsplit_encoder.seen);
    assert_eq!(split_vectors.len(), unsplit_prepared.vectors.len());
    for (split, unsplit) in split_vectors.iter().zip(&unsplit_prepared.vectors) {
        assert_eq!(split.chunk_id, unsplit.chunk_id);
        assert_eq!(split.chunk_digest, unsplit.chunk_digest);
        assert_eq!(split.values, unsplit.values);
        assert_eq!(split.output_digest, unsplit.output_digest);
    }
    assert_eq!(
        split_vectors
            .iter()
            .map(|vector| vector.output_digest.clone())
            .collect::<Vec<_>>(),
        unsplit_prepared
            .vectors
            .iter()
            .map(|vector| vector.output_digest.clone())
            .collect::<Vec<_>>()
    );
}

/// Count-valid groups can still exceed the admitted native-input byte ceiling.
/// Both projection-profile-change encoder lanes must use the same greedy,
/// data-dependent boundaries whether they are projected in one request or
/// paged into independent commits.
#[test]
fn profile_change_paging_preserves_count_and_byte_canonical_encoder_groups() {
    const INFERENCE_BATCH_BYTES: u32 = 64 * 1024;

    let previous_key = embedding_key();
    let previous_projection_key = previous_key.projection_key().expect("previous key");
    let mut replacement_key = previous_key.clone();
    replacement_key.model_artifact_digest = digest(b'8');
    replacement_key.inference_batch_bytes = INFERENCE_BATCH_BYTES;
    let replacement_projection_key = replacement_key.projection_key().expect("replacement key");
    let admitted = admitted_key(&replacement_key);
    let lane_chunks = |lane: &str, ordinal_offset: u32| {
        let large_text = "x".repeat(33 * 1024);
        let mut chunks = vec![
            chunk(
                "code-generation.2",
                &format!("byte-{lane}-00-large"),
                &large_text,
                ordinal_offset,
            ),
            chunk(
                "code-generation.2",
                &format!("byte-{lane}-01-large"),
                &large_text,
                ordinal_offset + 1,
            ),
        ];
        chunks.extend((2..9).map(|index| {
            chunk(
                "code-generation.2",
                &format!("byte-{lane}-{index:02}-small"),
                &format!("fn {lane}_{index}() {{}}"),
                ordinal_offset + index,
            )
        }));
        chunks.sort_by(|left, right| left.id.cmp(&right.id));
        chunks
    };
    let added_chunks = lane_chunks("added", 0);
    let reused_chunks = lane_chunks("reused", 9);
    let reprofile = request(
        changes(
            Some("code-generation.1"),
            "code-generation.2",
            added_chunks
                .iter()
                .map(|chunk| change(chunk, None, Some(chunk.content_digest.clone())))
                .collect(),
            vec![],
            reused_chunks
                .iter()
                .map(|chunk| {
                    change(
                        chunk,
                        Some(chunk.content_digest.clone()),
                        Some(chunk.content_digest.clone()),
                    )
                })
                .collect(),
        ),
        Some(previous_projection_key),
        replacement_projection_key,
        ProjectionReplayReasonV1::ProjectionProfileChange,
    );
    let mut canonical_chunks = added_chunks.clone();
    canonical_chunks.extend(reused_chunks.clone());
    let expected_groups = vec![
        vec![added_chunks[0].id.clone()],
        added_chunks[1..]
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect(),
        vec![reused_chunks[0].id.clone()],
        reused_chunks[1..]
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect(),
    ];

    let unsplit = split_projection_request(
        &reprofile,
        &canonical_chunks,
        4_096,
        8,
        INFERENCE_BATCH_BYTES as usize,
    )
    .expect("whole byte-bounded profile change");
    assert_eq!(unsplit.len(), 1);
    let mut unsplit_encoder = FakeEncoder::default();
    let unsplit_prepared = prepare_vector_generation(
        &admitted,
        unsplit[0].request.clone(),
        &unsplit[0].canonical_chunks,
        &mut unsplit_encoder,
    )
    .expect("whole byte-bounded profile change projection");
    assert_eq!(unsplit_encoder.batches, expected_groups);

    let split = split_projection_request(
        &reprofile,
        &canonical_chunks,
        8,
        8,
        INFERENCE_BATCH_BYTES as usize,
    )
    .expect("split byte-bounded profile change");
    assert_eq!(split.len(), 4);
    assert_eq!(
        split
            .iter()
            .map(|page| page.request.changes.added_or_changed.len())
            .collect::<Vec<_>>(),
        vec![1, 8, 0, 0]
    );
    assert_eq!(
        split
            .iter()
            .map(|page| page.request.changes.reused.len())
            .collect::<Vec<_>>(),
        vec![0, 0, 1, 8]
    );

    let mut split_encoder = FakeEncoder::default();
    let mut split_vectors = Vec::new();
    for batch in split {
        let prepared = prepare_vector_generation(
            &admitted,
            batch.request,
            &batch.canonical_chunks,
            &mut split_encoder,
        )
        .expect("split byte-bounded profile change projection");
        split_vectors.extend(prepared.vectors);
    }

    assert_eq!(split_encoder.batches, expected_groups);
    assert_eq!(split_encoder.batches, unsplit_encoder.batches);
    assert_eq!(split_encoder.seen, unsplit_encoder.seen);
    assert_eq!(split_vectors.len(), unsplit_prepared.vectors.len());
    for (split, unsplit) in split_vectors.iter().zip(&unsplit_prepared.vectors) {
        assert_eq!(split.chunk_id, unsplit.chunk_id);
        assert_eq!(split.chunk_digest, unsplit.chunk_digest);
        assert_eq!(split.values, unsplit.values);
        assert_eq!(split.output_digest, unsplit.output_digest);
    }
}

#[test]
fn interleaved_identical_files_keep_their_own_encoder_groups() {
    let key = embedding_key();
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let alpha = interleaved_file_chunks("code-generation.1", "alpha", "shared", 5);
    let beta = interleaved_file_chunks("code-generation.1", "beta", "shared", 5);
    let mut corpus = alpha.clone();
    corpus.extend(beta.clone());
    corpus.sort_by(|left, right| left.id.cmp(&right.id));
    let projection_request = whole_corpus_request(&corpus, &projection_key);
    let mut encoder = FakeEncoder::default();

    let prepared = prepare_vector_generation(&admitted, projection_request, &corpus, &mut encoder)
        .expect("interleaved files projection");

    assert_eq!(
        encoder.batches,
        vec![
            alpha
                .iter()
                .map(|chunk| chunk.id.clone())
                .collect::<Vec<_>>(),
            beta.iter()
                .map(|chunk| chunk.id.clone())
                .collect::<Vec<_>>(),
        ],
        "interleaved request IDs must not merge independent file tensors"
    );
    let alpha_values = alpha
        .iter()
        .map(|chunk| {
            prepared
                .vectors
                .iter()
                .find(|vector| vector.chunk_id == chunk.id)
                .expect("alpha vector")
                .values
                .clone()
        })
        .collect::<Vec<_>>();
    let beta_values = beta
        .iter()
        .map(|chunk| {
            prepared
                .vectors
                .iter()
                .find(|vector| vector.chunk_id == chunk.id)
                .expect("beta vector")
                .values
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        alpha_values, beta_values,
        "identical copied files must retain matching per-file tensor groups"
    );
}

#[test]
fn pagination_preserves_interleaved_file_bucket_groups_and_vector_identity() {
    let key = embedding_key();
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let alpha = interleaved_file_chunks("code-generation.1", "alpha", "shared", 5);
    let beta = interleaved_file_chunks("code-generation.1", "beta", "shared", 5);
    let mut corpus = alpha.clone();
    corpus.extend(beta.clone());
    corpus.sort_by(|left, right| left.id.cmp(&right.id));
    let whole = whole_corpus_request(&corpus, &projection_key);
    let expected_groups = vec![
        alpha
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>(),
        beta.iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>(),
    ];

    let unsplit = split_projection_request(
        &whole,
        &corpus,
        4_096,
        key.inference_batch_size as usize,
        key.inference_batch_bytes as usize,
    )
    .expect("unsplit request");
    let mut unsplit_encoder = FakeEncoder::default();
    let unsplit_prepared = prepare_vector_generation(
        &admitted,
        unsplit[0].request.clone(),
        &unsplit[0].canonical_chunks,
        &mut unsplit_encoder,
    )
    .expect("unsplit file-bucket projection");

    let split = split_projection_request(
        &whole,
        &corpus,
        8,
        key.inference_batch_size as usize,
        key.inference_batch_bytes as usize,
    )
    .expect("split request");
    assert_eq!(split.len(), 2);
    assert_eq!(
        split
            .iter()
            .map(|page| {
                page.request
                    .changes
                    .added_or_changed
                    .iter()
                    .map(|change| change.chunk_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        expected_groups,
        "a page must carry each file's whole canonical encoder groups"
    );

    let mut split_encoder = FakeEncoder::default();
    let mut split_vectors = Vec::new();
    for batch in split {
        let prepared = prepare_vector_generation(
            &admitted,
            batch.request,
            &batch.canonical_chunks,
            &mut split_encoder,
        )
        .expect("split file-bucket projection");
        split_vectors.extend(prepared.vectors);
    }

    assert_eq!(unsplit_encoder.batches, expected_groups);
    assert_eq!(split_encoder.batches, unsplit_encoder.batches);
    split_vectors.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    let mut unsplit_vectors = unsplit_prepared.vectors;
    unsplit_vectors.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    for (split, unsplit) in split_vectors.iter().zip(&unsplit_vectors) {
        assert_eq!(split.chunk_id, unsplit.chunk_id);
        assert_eq!(split.chunk_digest, unsplit.chunk_digest);
        assert_eq!(split.values, unsplit.values);
        assert_eq!(split.output_digest, unsplit.output_digest);
    }
}

#[test]
fn preceding_unrelated_file_does_not_change_later_file_groups_or_vectors() {
    let key = embedding_key();
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let later = interleaved_file_chunks("code-generation.1", "later", "shared", 5);
    let later_request = whole_corpus_request(&later, &projection_key);
    let mut later_encoder = FakeEncoder {
        batch_shape_sensitive: true,
        ..FakeEncoder::default()
    };
    let later_prepared =
        prepare_vector_generation(&admitted, later_request, &later, &mut later_encoder)
            .expect("later-file-only projection");

    let preceding = interleaved_file_chunks("code-generation.1", "before", "unrelated", 3);
    let mut extended = preceding.clone();
    extended.extend(later.clone());
    extended.sort_by(|left, right| left.id.cmp(&right.id));
    let extended_request = whole_corpus_request(&extended, &projection_key);
    let mut extended_encoder = FakeEncoder {
        batch_shape_sensitive: true,
        ..FakeEncoder::default()
    };
    let extended_prepared = prepare_vector_generation(
        &admitted,
        extended_request,
        &extended,
        &mut extended_encoder,
    )
    .expect("preceding-file projection");

    assert_eq!(
        extended_encoder.batches,
        vec![
            preceding
                .iter()
                .map(|chunk| chunk.id.clone())
                .collect::<Vec<_>>(),
            later
                .iter()
                .map(|chunk| chunk.id.clone())
                .collect::<Vec<_>>(),
        ],
        "the preceding file must not share a tensor with the later file"
    );
    let extended_later_vectors = extended_prepared
        .vectors
        .iter()
        .filter(|vector| later.iter().any(|chunk| chunk.id == vector.chunk_id))
        .collect::<Vec<_>>();
    assert_eq!(extended_later_vectors.len(), later_prepared.vectors.len());
    for (extended, later_only) in extended_later_vectors.iter().zip(&later_prepared.vectors) {
        assert_eq!(extended.chunk_id, later_only.chunk_id);
        assert_eq!(extended.values, later_only.values);
        assert_eq!(extended.output_digest, later_only.output_digest);
    }
}

#[test]
fn projection_rejects_a_chunk_that_exceeds_its_admitted_byte_ceiling() {
    let mut key = embedding_key();
    key.inference_batch_bytes = 4;
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let oversized = chunk("code-generation.1", "oversized", "abcde", 0);
    let projection_request = request(
        changes(
            None,
            "code-generation.1",
            vec![change(
                &oversized,
                None,
                Some(oversized.content_digest.clone()),
            )],
            vec![],
            vec![],
        ),
        None,
        projection_key,
        ProjectionReplayReasonV1::InitialProjection,
    );
    let mut encoder = FakeEncoder::default();

    let error = prepare_vector_generation(
        &admitted,
        projection_request,
        &[oversized.clone()],
        &mut encoder,
    )
    .expect_err("a chunk over the admitted ceiling must not reach the encoder");

    assert_eq!(
        error,
        SemanticProjectionErrorV1::InferenceBatchByteCeilingExceeded {
            chunk_id: oversized.id,
            actual_bytes: 5,
            inference_batch_bytes: 4,
        }
    );
    assert!(encoder.seen.is_empty());
}

/// Incremental reuse used to ride on the last (or only) embed page. A 10x
/// measurement then spent a corpus-sized mutation/capacity budget on one
/// receipt. Page reused with the same window as embeds.
#[test]
fn incremental_reused_chunks_are_paged_with_the_embed_window() {
    let key = embedding_key();
    let projection_key = key.projection_key().expect("projection key");
    let added_chunk = chunk("code-generation.2", "added", "new symbol", 0);
    let mut reused_chunks = (0..40)
        .map(|index| {
            chunk(
                "code-generation.2",
                &format!("reused-{index:02}"),
                "same text",
                index + 1,
            )
        })
        .collect::<Vec<_>>();
    reused_chunks.sort_by(|left, right| left.id.cmp(&right.id));
    let reused = reused_chunks
        .iter()
        .map(|chunk| {
            change(
                chunk,
                Some(chunk.content_digest.clone()),
                Some(chunk.content_digest.clone()),
            )
        })
        .collect();
    let incremental = request(
        changes(
            Some("code-generation.1"),
            "code-generation.2",
            vec![change(
                &added_chunk,
                None,
                Some(added_chunk.content_digest.clone()),
            )],
            vec![],
            reused,
        ),
        Some(projection_key.clone()),
        projection_key,
        ProjectionReplayReasonV1::SourceEdit,
    );
    let pages = split_projection_request(
        &incremental,
        &[added_chunk],
        16,
        8,
        key.inference_batch_bytes as usize,
    )
    .expect("split reused");
    assert_eq!(pages.len(), 3, "1 added + 40 reused must page at window 16");
    assert!(
        pages.iter().all(|page| {
            page.request.changes.added_or_changed.len()
                + page.request.changes.deleted.len()
                + page.request.changes.reused.len()
                <= 16
        }),
        "no page may exceed the named window"
    );
    assert_eq!(pages[0].request.changes.added_or_changed.len(), 1);
    assert_eq!(pages[0].request.changes.reused.len(), 15);
    assert!(pages[1].request.changes.added_or_changed.is_empty());
    assert_eq!(pages[1].request.changes.reused.len(), 16);
    assert!(pages[2].request.changes.added_or_changed.is_empty());
    assert_eq!(pages[2].request.changes.reused.len(), 9);
    assert_eq!(pages[0].canonical_chunks.len(), 1);
    assert!(pages[1].canonical_chunks.is_empty());
    assert!(pages[2].canonical_chunks.is_empty());
}

/// A run that stops partway resumes from its durable checkpoint.
#[test]
fn a_partial_incremental_run_resumes_from_its_checkpoint() {
    let key = embedding_key();
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let corpus = split_identity_corpus();
    let whole = whole_corpus_request(&corpus, &projection_key);
    let mut expected_chunk_ids = corpus
        .iter()
        .map(|chunk| chunk.id.clone())
        .collect::<Vec<_>>();
    expected_chunk_ids.sort();
    let plan = VectorGenerationPlanV1 {
        target_projection_key: projection_key,
        source_generation: id("code-generation.1"),
        source_manifest_digest: whole.changes.manifest_digest.clone(),
        expected_chunk_ids: expected_chunk_ids.into(),
        base_generation: None,
    };
    let batches =
        split_projection_request(&whole, &corpus, 16, 8, key.inference_batch_bytes as usize)
            .expect("split request");
    let reference = publish_in_batches(&admitted, plan.clone(), batches.clone()).1;

    let mut store = VectorGenerationStateMachineV1::new();
    let build = store.begin_generation(plan.clone()).expect("staged build");
    let mut checkpoint = None;
    for batch in batches.iter().take(1) {
        let prepared = prepare_vector_generation(
            &admitted,
            batch.request.clone(),
            &batch.canonical_chunks,
            &mut FakeEncoder::default(),
        )
        .expect("projection batch");
        checkpoint = Some(
            store
                .commit_batch(&build, checkpoint.as_ref(), prepared)
                .expect("batch commit"),
        );
    }
    assert_eq!(
        store.publish_generation(&build),
        Err(VectorGenerationStoreErrorV1::IncompleteGeneration),
        "a partial run must never become the active generation"
    );

    // Restart: the build identity is a digest of the plan, so reopening the
    // same plan re-adopts the same staged build, and its checkpoint says how
    // many batches are already durable.
    let resumed_build = store.begin_generation(plan).expect("resumed build");
    assert_eq!(resumed_build, build, "the staged build is re-adopted");
    let resumed = store
        .staged_checkpoint(&resumed_build)
        .expect("staged checkpoint")
        .clone();
    assert_eq!(
        resumed.completed_batches, 1,
        "the checkpoint names exactly the batches that committed"
    );

    let mut checkpoint = Some(resumed);
    for batch in batches.into_iter().skip(1) {
        let prepared = prepare_vector_generation(
            &admitted,
            batch.request,
            &batch.canonical_chunks,
            &mut FakeEncoder::default(),
        )
        .expect("projection batch");
        checkpoint = Some(
            store
                .commit_batch(&resumed_build, checkpoint.as_ref(), prepared)
                .expect("resumed batch commit"),
        );
    }
    let publication = store
        .publish_generation(&resumed_build)
        .expect("resumed publication");
    assert_eq!(
        publication.generation_id, reference.generation_id,
        "resuming publishes the same generation an uninterrupted run would"
    );
    assert_eq!(publication.manifest_digest, reference.manifest_digest);
}

#[tokio::test]
async fn graph_store_survives_reopen_and_preserves_superseded_generations() {
    let key = embedding_key();
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");
    let alpha_v1 = chunk("code-generation.1", "alpha", "fn alpha() -> u8 { 1 }", 0);
    let stable_v1 = chunk("code-generation.1", "stable", "fn stable() {}", 1);
    let initial = prepare_vector_generation(
        &admitted,
        request(
            changes(
                None,
                "code-generation.1",
                vec![
                    change(&alpha_v1, None, Some(alpha_v1.content_digest.clone())),
                    change(&stable_v1, None, Some(stable_v1.content_digest.clone())),
                ],
                vec![],
                vec![],
            ),
            None,
            projection_key.clone(),
            ProjectionReplayReasonV1::InitialProjection,
        ),
        &[alpha_v1.clone(), stable_v1.clone()],
        &mut FakeEncoder::default(),
    )
    .expect("initial projection");
    let mut store = VectorGenerationStateMachineV1::new();
    let mut initial_chunk_ids = vec![alpha_v1.id.clone(), stable_v1.id.clone()];
    initial_chunk_ids.sort();
    let initial_build = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: id("code-generation.1"),
            source_manifest_digest: initial.receipt.source_manifest_digest.clone(),
            expected_chunk_ids: initial_chunk_ids.into(),
            base_generation: None,
        })
        .expect("initial build");
    store
        .commit_batch(&initial_build, None, initial)
        .expect("initial batch");
    let initial_publication = store
        .publish_generation(&initial_build)
        .expect("initial publication");

    let encoded = store.persist_sealed().expect("persist vector state");
    let mut restarted =
        VectorGenerationStateMachineV1::reopen_sealed(&encoded).expect("reopen vector state");
    assert_eq!(
        restarted
            .generation(&initial_publication.generation_id)
            .expect("recovered initial generation")
            .vectors()
            .len(),
        2
    );

    let stable_v2 = chunk("code-generation.2", "stable", "fn stable() {}", 0);
    let next = prepare_vector_generation(
        &admitted,
        request(
            changes(
                Some("code-generation.1"),
                "code-generation.2",
                vec![],
                vec![change(
                    &alpha_v1,
                    Some(alpha_v1.content_digest.clone()),
                    None,
                )],
                vec![change(
                    &stable_v2,
                    Some(stable_v1.content_digest.clone()),
                    Some(stable_v2.content_digest.clone()),
                )],
            ),
            Some(projection_key.clone()),
            projection_key.clone(),
            ProjectionReplayReasonV1::SourceEdit,
        ),
        &[],
        &mut FakeEncoder::default(),
    )
    .expect("deletion and reuse projection");
    let next_plan = VectorGenerationPlanV1 {
        target_projection_key: projection_key,
        source_generation: id("code-generation.2"),
        source_manifest_digest: next.receipt.source_manifest_digest.clone(),
        expected_chunk_ids: vec![stable_v2.id.clone()].into(),
        base_generation: Some(initial_publication.generation_id.clone()),
    };
    let next_build = restarted
        .begin_generation(next_plan.clone())
        .expect("superseding build");
    restarted
        .commit_batch(&next_build, None, next.clone())
        .expect("superseding batch");
    let next_publication = restarted
        .publish_generation(&next_build)
        .expect("superseding publication");
    let current = restarted
        .generation(&next_publication.generation_id)
        .expect("new immutable generation");
    assert_eq!(
        current.base_generation(),
        Some(&initial_publication.generation_id)
    );
    assert_eq!(
        current.vectors().keys().collect::<Vec<_>>(),
        vec![&stable_v2.id]
    );
    assert_eq!(current.tombstones(), &[alpha_v1.id.clone()]);
    assert!(
        restarted
            .generation(&initial_publication.generation_id)
            .expect("superseded generation remains exact-addressable")
            .vectors()
            .contains_key(&alpha_v1.id)
    );

    let encoded = restarted
        .persist_sealed()
        .expect("persist superseded generations");
    let reopened = VectorGenerationStateMachineV1::reopen_sealed(&encoded).expect("second reopen");
    assert!(
        reopened
            .generation(&initial_publication.generation_id)
            .expect("historical generation after restart")
            .vectors()
            .contains_key(&alpha_v1.id)
    );
    assert_eq!(
        reopened
            .generation(&next_publication.generation_id)
            .expect("new generation after restart")
            .vectors()
            .keys()
            .collect::<Vec<_>>(),
        vec![&stable_v2.id]
    );
}

/// An encoder whose declared width changes how the projector windows and
/// dispatches groups, while the values it produces stay a pure function of the
/// chunk. Groups are executed out of order on purpose (last group first) so any
/// dependence of the output on execution order surfaces as a failure.
struct WidthEncoder {
    concurrency: usize,
    groups: std::sync::Arc<std::sync::Mutex<Vec<Vec<CodeSearchChunkId>>>>,
}

impl WidthEncoder {
    fn new(concurrency: usize) -> Self {
        Self {
            concurrency,
            groups: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Group compositions, sorted so the comparison is about *which* chunks
    /// share a tensor — not the order the encoder happened to run them in,
    /// which this fixture deliberately scrambles.
    fn observed_groups(&self) -> Vec<Vec<CodeSearchChunkId>> {
        let mut groups = self.groups.lock().expect("group log").clone();
        groups.sort();
        groups
    }
}

impl CanonicalChunkVectorEncoderV1 for WidthEncoder {
    fn encode(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<Vec<f32>, String> {
        let seed = chunk
            .sanitized_text
            .as_str()
            .bytes()
            .fold(0u32, |sum, byte| sum.wrapping_add(u32::from(byte)));
        Ok((0..key.dimensions as usize)
            .map(|index| (seed.wrapping_add(index as u32) % 101) as f32 / 101.0)
            .collect())
    }

    fn encode_batch(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<Vec<f32>>, String> {
        self.groups
            .lock()
            .expect("group log")
            .push(chunks.iter().map(|chunk| chunk.id.clone()).collect());
        chunks.iter().map(|chunk| self.encode(key, chunk)).collect()
    }

    fn encode_concurrency(&self) -> usize {
        self.concurrency
    }

    fn encode_batches(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        groups: &[&[&CodeSearchChunkV1]],
    ) -> Result<Vec<Vec<Vec<f32>>>, String> {
        let mut encoded = groups
            .iter()
            .rev()
            .map(|group| self.encode_batch(key, group))
            .collect::<Result<Vec<_>, _>>()?;
        encoded.reverse();
        Ok(encoded)
    }
}

#[test]
fn projection_vectors_are_byte_identical_at_every_encoder_width() {
    let mut key = embedding_key();
    key.inference_batch_size = 32;
    let admitted = admitted_key(&key);
    let projection_key = key.projection_key().expect("projection key");

    // More chunks than one dispatch window holds at width 1, so the narrow run
    // really does span several windows while the wide runs do not.
    let corpus = (0..97u32)
        .map(|index| {
            chunk(
                "code-generation.1",
                // Zero-padded so lexicographic chunk-id order matches the
                // numeric order the canonical changed set requires.
                &format!("chunk{index:04}"),
                &format!("fn chunk_{index}() -> u32 {{ {index} }}"),
                index,
            )
        })
        .collect::<Vec<_>>();
    let build_request = || {
        request(
            changes(
                None,
                "code-generation.1",
                corpus
                    .iter()
                    .map(|chunk| change(chunk, None, Some(chunk.content_digest.clone())))
                    .collect(),
                vec![],
                vec![],
            ),
            None,
            projection_key.clone(),
            ProjectionReplayReasonV1::InitialProjection,
        )
    };

    let mut narrow_encoder = WidthEncoder::new(1);
    let narrow =
        prepare_vector_generation(&admitted, build_request(), &corpus, &mut narrow_encoder)
            .expect("width-1 projection");

    for width in [2usize, 8, 64] {
        let mut wide_encoder = WidthEncoder::new(width);
        let wide =
            prepare_vector_generation(&admitted, build_request(), &corpus, &mut wide_encoder)
                .expect("wide projection");
        assert_eq!(
            narrow.vectors, wide.vectors,
            "width {width} changed the projected vectors"
        );
        assert_eq!(
            narrow.receipt, wide.receipt,
            "width {width} changed the projection receipt"
        );
        assert_eq!(
            narrow_encoder.observed_groups(),
            wide_encoder.observed_groups(),
            "width {width} regrouped the encoder batches, which would change tensor shape"
        );
    }

    let groups = narrow_encoder.observed_groups();
    assert_eq!(
        groups.iter().map(Vec::len).sum::<usize>(),
        corpus.len(),
        "every chunk is encoded exactly once"
    );
    assert_eq!(
        groups.iter().filter(|group| group.len() != 32).count(),
        1,
        "the admitted production batch shape is 32 — only the final short group may \
         differ, so the tensor shape never depends on the dispatch window"
    );
}
