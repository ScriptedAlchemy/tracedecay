#![allow(clippy::cloned_ref_to_slice_refs, clippy::drop_non_drop)] // test builders and explicit early drops
#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::sync::mpsc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay::code_index::projection::{expected_request_digest, verify_batch_receipt};
use tracedecay::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay::store::vector_generation_test_support::{
    CanonicalChunkVectorEncoderV1, DatabaseVectorGenerationStoreV1, FakeVectorGenerationStoreV1,
    ProjectionRequestBatchV1, SemanticProjectionErrorV1, VectorGenerationIdV1,
    VectorGenerationPlanV1, VectorGenerationStoreErrorV1, fail_before_publication_swap_once,
    prepare_vector_generation, prepare_vector_generation_async, split_projection_request,
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
        chunks.iter().map(|chunk| self.encode(key, chunk)).collect()
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
    FakeVectorGenerationStoreV1,
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

    let mut store = FakeVectorGenerationStoreV1::new();
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
        .publish_generation(&build_id, None)
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
    assert_eq!(store.active_generation_id(), Some(&base_generation));
    assert!(
        store
            .active_generation_for(&admitted, &prior_source_generation, &prior_source_manifest,)
            .is_some(),
        "an exactly compatible prior snapshot remains queryable while indexing"
    );
    assert!(
        store
            .active_generation_for(&admitted, &id("code-generation.2"), &next_source_manifest,)
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
    assert_eq!(store.active_generation_id(), Some(&base_generation));
    assert!(store.cancel_generation(&build_id));
    assert_eq!(store.active_generation_id(), Some(&base_generation));
    assert_eq!(
        store.publish_generation(&build_id, Some(&base_generation)),
        Err(VectorGenerationStoreErrorV1::UnknownBuild)
    );
}

#[test]
fn semantic_projection_key_is_complete_deterministic_and_maps_to_plan25() {
    let key = embedding_key();
    key.validate().expect("valid key");
    let first = key.canonical_digest().expect("key digest");
    let second = key.canonical_digest().expect("stable replay");
    assert_eq!(first, second);

    let generic = key.projection_key().expect("generic Plan25 key");
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
fn fake_projection_uses_canonical_chunks_and_plan25_receipts() {
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
    verify_batch_receipt(&prepared.request, &prepared.receipt).expect("Plan25 receipt");

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
    assert_eq!(store.active_generation_id(), Some(&base_generation));

    let publication = store
        .publish_generation(&build_id, Some(&base_generation))
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
fn checkpoint_and_active_pointer_publish_atomically() {
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

    let prior_checkpoint = store.active_checkpoint().cloned();
    fail_before_publication_swap_once(&mut store);
    assert_eq!(
        store.publish_generation(&build_id, Some(&base_generation)),
        Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure)
    );
    assert_eq!(store.active_generation_id(), Some(&base_generation));
    assert_eq!(store.active_checkpoint(), prior_checkpoint.as_ref());

    let publication = store
        .publish_generation(&build_id, Some(&base_generation))
        .expect("retry publishes atomically");
    assert_eq!(
        store.active_generation_id(),
        Some(&publication.generation_id)
    );
    assert_eq!(store.active_checkpoint(), Some(&publication.checkpoint));
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
    let published = store
        .publish_generation(&build_id, Some(&base_generation))
        .unwrap();
    assert_eq!(
        store
            .generation(&published.generation_id)
            .unwrap()
            .vectors()
            .len(),
        3
    );
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
    let mut store = FakeVectorGenerationStoreV1::new();
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
    let mut single_store = FakeVectorGenerationStoreV1::new();
    let single_build = single_store
        .begin_generation(plan.clone())
        .expect("single build");
    single_store
        .commit_batch(&single_build, None, single_prepared)
        .expect("single batch commit");
    let single = single_store
        .publish_generation(&single_build, None)
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
    let mut multi_store = FakeVectorGenerationStoreV1::new();
    let multi_build = multi_store.begin_generation(plan).expect("multi build");
    let checkpoint = multi_store
        .commit_batch(&multi_build, None, alpha_prepared)
        .expect("first batch commit");
    assert_eq!(
        multi_store.publish_generation(&multi_build, None),
        Err(VectorGenerationStoreErrorV1::IncompleteGeneration),
        "a partial batch checkpoint must never become the active generation"
    );
    assert_eq!(multi_store.active_generation_id(), None);
    multi_store
        .commit_batch(&multi_build, Some(&checkpoint), beta_prepared)
        .expect("second batch commit");
    let multi = multi_store
        .publish_generation(&multi_build, None)
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
    FakeVectorGenerationStoreV1,
    tracedecay::store::vector_generation_test_support::VectorGenerationPublicationV1,
) {
    let mut store = FakeVectorGenerationStoreV1::new();
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
    let publication = store.publish_generation(&build, None).expect("publication");
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

    let unsplit = split_projection_request(&whole, &corpus, 4_096).expect("unsplit request");
    assert_eq!(
        unsplit.len(),
        1,
        "a corpus inside one batch window is not split at all"
    );
    let (single_store, single) = publish_in_batches(&admitted, plan.clone(), unsplit);

    // 16 embeds per batch is two encoder groups, so boundaries stay aligned.
    let split = split_projection_request(&whole, &corpus, 16).expect("split request");
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
    let batches = split_projection_request(&whole, &corpus, 16).expect("split request");
    let reference = publish_in_batches(&admitted, plan.clone(), batches.clone()).1;

    let mut store = FakeVectorGenerationStoreV1::new();
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
        store.publish_generation(&build, None),
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
        .publish_generation(&resumed_build, None)
        .expect("resumed publication");
    assert_eq!(
        publication.generation_id, reference.generation_id,
        "resuming publishes the same generation an uninterrupted run would"
    );
    assert_eq!(publication.manifest_digest, reference.manifest_digest);
}

#[tokio::test]
async fn database_store_survives_restart_and_preserves_superseded_generations() {
    let temp = tempfile::tempdir().expect("temporary project store");
    let database_path = temp.path().join("project.db");
    let authority =
        DatabaseAuthority::acquire_test(&database_path, "vector generation restart test")
            .expect("project database authority");
    let database = Database::publish_test_runtime(
        &database_path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("project database")
    .0;
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
    let store = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("persistent vector store");
    let initial_build = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: id("code-generation.1"),
            source_manifest_digest: initial.receipt.source_manifest_digest.clone(),
            expected_chunk_ids: vec![alpha_v1.id.clone(), stable_v1.id.clone()].into(),
            base_generation: None,
        })
        .await
        .expect("initial persistent build");
    store
        .commit_batch(&initial_build, None, initial)
        .await
        .expect("initial persistent batch");
    drop(store);

    let resumed = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("resume checkpointed vector build");
    assert_eq!(
        resumed.active_generation_id().await.unwrap(),
        None,
        "checkpointed partial generations remain unqueryable"
    );
    let initial_publication = resumed
        .publish_generation(&initial_build, None)
        .await
        .expect("publish resumed persistent generation");
    drop(resumed);

    let restarted = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("restart persistent vector store");
    assert_eq!(
        restarted.active_generation_id().await.unwrap(),
        Some(initial_publication.generation_id.clone())
    );
    assert_eq!(
        restarted
            .generation(&initial_publication.generation_id)
            .await
            .unwrap()
            .expect("initial immutable generation")
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
        .await
        .expect("superseding build");
    restarted
        .commit_batch(&next_build, None, next.clone())
        .await
        .expect("superseding batch");
    let next_publication = restarted
        .publish_generation(&next_build, Some(&initial_publication.generation_id))
        .await
        .expect("atomic supersession");

    let current = restarted
        .generation(&next_publication.generation_id)
        .await
        .unwrap()
        .expect("current immutable generation");
    assert_eq!(
        current.base_generation(),
        Some(&initial_publication.generation_id)
    );
    assert_eq!(
        current.vectors().keys().collect::<Vec<_>>(),
        vec![&stable_v2.id]
    );
    assert_eq!(current.tombstones(), &[alpha_v1.id.clone()]);
    assert_eq!(
        current.tombstone_digests().get(&alpha_v1.id),
        Some(&alpha_v1.content_digest)
    );
    assert!(
        restarted
            .generation(&initial_publication.generation_id)
            .await
            .unwrap()
            .expect("superseded generation remains addressable")
            .vectors()
            .contains_key(&alpha_v1.id)
    );

    drop(restarted);
    let rebuilt = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("second restart");
    assert_eq!(
        rebuilt.active_generation_id().await.unwrap(),
        Some(next_publication.generation_id.clone())
    );
    let replay_build = rebuilt
        .rebuild_generation(next_plan.clone())
        .await
        .expect("restart deterministic rebuild from query inputs");
    rebuilt
        .commit_batch(&replay_build, None, next.clone())
        .await
        .expect("lost-ack replay");
    assert_eq!(
        rebuilt.active_generation_id().await.unwrap(),
        Some(next_publication.generation_id.clone()),
        "rebuild staging cannot expose a partial generation"
    );
    assert!(
        rebuilt
            .cancel_generation(&replay_build)
            .await
            .expect("cancel staged rebuild")
    );
    assert_eq!(
        rebuilt.active_generation_id().await.unwrap(),
        Some(next_publication.generation_id.clone()),
        "cancelling a rebuild preserves the prior active pointer"
    );
    let replay_build = rebuilt
        .rebuild_generation(next_plan)
        .await
        .expect("restart deterministic rebuild after cancellation");
    rebuilt
        .commit_batch(&replay_build, None, next)
        .await
        .expect("replay after cancellation");
    let replay = rebuilt
        .publish_generation(&replay_build, Some(&next_publication.generation_id))
        .await
        .expect("deterministic rebuild publication");
    assert_eq!(replay.generation_id, next_publication.generation_id);

    let restored = rebuilt
        .activate_generation(
            &initial_publication.generation_id,
            Some(&next_publication.generation_id),
        )
        .await
        .expect("rollback to immutable prior generation");
    assert_eq!(restored.generation_id, initial_publication.generation_id);
    assert_eq!(
        rebuilt.active_generation_id().await.unwrap(),
        Some(initial_publication.generation_id),
        "rollback atomically restores the prior active pointer"
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
    let key = embedding_key();
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
            "width {width} changed the Plan 25 receipt"
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
        groups.iter().filter(|group| group.len() != 8).count(),
        1,
        "groups are fixed-size — only the final short group may differ, so the \
         tensor shape never depends on the window"
    );
}
