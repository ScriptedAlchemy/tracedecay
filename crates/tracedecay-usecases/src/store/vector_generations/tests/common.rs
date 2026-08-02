fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("canonical test identity")
}

fn manifest_digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn content_digest(byte: char) -> ContentDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn canonical_chunk(
    chunk_id: &str,
    source_generation: &CodeGenerationId,
    digest: char,
) -> tracedecay_domain::CodeSearchChunkV1 {
    tracedecay_domain::CodeSearchChunkV1 {
        id: id(chunk_id),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: source_generation.clone(),
            file_occurrence_id: id::<FileOccurrenceId>("file.rs"),
            symbol_occurrence_id: None,
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: 4,
            },
            grain: CodeSearchChunkGrainV1::FileWindow,
            ordinal: 0,
        },
        content_digest: content_digest(digest),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: id::<PolicyRevisionId>("policy.v1"),
        },
        exact_terms: vec![],
        subtokens: vec![],
        sanitized_text: BoundedSanitizedText::new("code").expect("sanitized text"),
    }
}

fn admitted_embedding() -> AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: manifest_digest('1'),
        tokenizer_digest: manifest_digest('2'),
        config_digest: manifest_digest('3'),
        query_instruction_digest: Some(manifest_digest('4')),
        document_instruction_digest: Some(manifest_digest('5')),
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "ort-test-rev-1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 1,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.project-a"),
        privacy_key_epoch: 7,
    }
    .admit()
    .expect("admitted embedding fixture")
}

fn admitted_embedding_for(
    privacy_domain: &str,
    privacy_key_epoch: u64,
    runtime_build_revision: &str,
) -> AdmittedEmbeddingProjectionKeyV1 {
    let mut key = admitted_embedding().embedding_key().clone();
    key.privacy_domain = id(privacy_domain);
    key.privacy_key_epoch = privacy_key_epoch;
    key.runtime_build_revision = runtime_build_revision.to_owned();
    key.admit().expect("admitted embedding fixture variant")
}

fn logical_generation(
    generation_digest: char,
    embedding_key: AdmittedEmbeddingProjectionKeyV1,
    source_generation: &str,
    source_manifest_digest: char,
    chunk_id: &str,
    chunk_digest: char,
    values: Vec<f32>,
) -> PublishedVectorGenerationV1 {
    let projection_key = embedding_key.projection_key().clone();
    let source_generation: CodeGenerationId = id(source_generation);
    let source_manifest_digest = manifest_digest(source_manifest_digest);
    let chunk_id: CodeSearchChunkId = id(chunk_id);
    let chunk_digest = content_digest(chunk_digest);
    let output_digest = tracedecay_semantic::projector::vector_output_digest(
        &projection_key,
        &chunk_id,
        &chunk_digest,
        &values,
    )
    .expect("canonical vector output digest");
    let vectors = BTreeMap::from([(
        chunk_id.clone(),
        ProjectedChunkVectorV1 {
            projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            chunk_id: chunk_id.clone(),
            chunk_digest: chunk_digest.clone(),
            values,
            output_digest: output_digest.clone(),
        },
    )]);
    let plan = VectorGenerationPlanV1 {
        target_projection_key: projection_key.clone(),
        source_generation: source_generation.clone(),
        source_manifest_digest: source_manifest_digest.clone(),
        expected_chunk_ids: vec![chunk_id.clone()].into(),
        base_generation: None,
    };
    let manifest_digest =
        generation_identity_digest(&plan, &vectors, &BTreeMap::new()).expect("manifest digest");
    let generation_id = VectorGenerationIdV1::new(manifest_digest.clone());
    let request_digest = manifest_digest_for_test_request(generation_digest);
    let mut batch = ProjectionBatchReceiptV1 {
        target_projection_key: projection_key.clone(),
        request_digest: request_digest.clone(),
        source_generation: source_generation.clone(),
        source_manifest_digest: source_manifest_digest.clone(),
        receipts: vec![tracedecay_domain::CodeChunkProjectionReceiptV1 {
            projection_key: projection_key.clone(),
            request_digest: request_digest.clone(),
            prior_generation: None,
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            chunk_id,
            prior_chunk_digest: None,
            current_chunk_digest: Some(chunk_digest),
            operation: ProjectionOperationV1::Added,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: Some(output_digest),
        }],
        reused_count: 0,
        publication_digest: manifest_digest_for_test_request('0'),
    };
    batch.publication_digest = expected_publication_digest(&batch).expect("publication digest");
    let publication_digest = batch.publication_digest.clone();
    PublishedVectorGenerationV1 {
        generation_id: generation_id.clone(),
        projection_key: projection_key.clone(),
        source_generation: source_generation.clone(),
        source_manifest_digest: source_manifest_digest.clone(),
        base_generation: None,
        embedding_key,
        vectors: vectors.into(),
        tombstones: Vec::new().into(),
        tombstone_digests: BTreeMap::new().into(),
        receipts: vec![batch].into(),
        checkpoint: VectorProjectionCheckpointV1 {
            target_projection_key: projection_key,
            source_generation,
            source_manifest_digest,
            completed_batches: 1,
            last_request_digest: Some(request_digest),
            last_publication_digest: Some(publication_digest),
        },
        manifest_digest,
    }
}

fn manifest_digest_for_test_request(byte: char) -> ManifestDigest {
    manifest_digest(if byte.is_ascii_hexdigit() { byte } else { 'f' })
}

fn reused_prepared(
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    from_generation: &CodeGenerationId,
    to_generation: &CodeGenerationId,
    chunk_id: &CodeSearchChunkId,
    chunk_digest: &ContentDigest,
) -> PreparedVectorGenerationV1 {
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: Some(from_generation.clone()),
        to_generation: to_generation.clone(),
        manifest_digest: manifest_digest('0'),
        added_or_changed: vec![],
        deleted: vec![],
        reused: vec![ChangedCodeChunkV1 {
            chunk_id: chunk_id.clone(),
            prior_digest: Some(chunk_digest.clone()),
            current_digest: Some(chunk_digest.clone()),
        }],
    };
    changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
    let mut request = ProjectionBatchRequestV1 {
        request_digest: manifest_digest('0'),
        changes,
        previous_projection_key: Some(embedding_key.projection_key().clone()),
        target_projection_key: embedding_key.projection_key().clone(),
        replay_reason: ProjectionReplayReasonV1::SourceEdit,
    };
    request.request_digest = tracedecay_code_index::projection::expected_request_digest(&request)
        .expect("projection request digest");
    let receipt = tracedecay_code_index::projection::build_batch_receipt(
        &request,
        &[
            tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                chunk_id: chunk_id.clone(),
                prior_chunk_digest: Some(chunk_digest.clone()),
                current_chunk_digest: Some(chunk_digest.clone()),
                operation: ProjectionOperationV1::Reused,
                outcome: ProjectionOutcomeV1::Reused,
                output_digest: None,
            },
        ],
    )
    .expect("reused projection receipt");
    PreparedVectorGenerationV1 {
        embedding_key: embedding_key.clone(),
        request,
        receipt,
        vectors: vec![],
        tombstones: vec![],
    }
}

/// One projection batch that adds a single chunk vector.
fn added_prepared(
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    to_generation: &CodeGenerationId,
    chunk_id: &CodeSearchChunkId,
    chunk_digest: &ContentDigest,
    values: Vec<f32>,
) -> PreparedVectorGenerationV1 {
    let projection_key = embedding_key.projection_key().clone();
    let output_digest = tracedecay_semantic::projector::vector_output_digest(
        &projection_key,
        chunk_id,
        chunk_digest,
        &values,
    )
    .expect("canonical vector output digest");
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: to_generation.clone(),
        manifest_digest: manifest_digest('0'),
        added_or_changed: vec![ChangedCodeChunkV1 {
            chunk_id: chunk_id.clone(),
            prior_digest: None,
            current_digest: Some(chunk_digest.clone()),
        }],
        deleted: vec![],
        reused: vec![],
    };
    changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
    let source_manifest_digest = changes.manifest_digest.clone();
    let mut request = ProjectionBatchRequestV1 {
        request_digest: manifest_digest('0'),
        changes,
        previous_projection_key: None,
        target_projection_key: projection_key.clone(),
        replay_reason: ProjectionReplayReasonV1::SourceEdit,
    };
    request.request_digest = tracedecay_code_index::projection::expected_request_digest(&request)
        .expect("projection request digest");
    let receipt = tracedecay_code_index::projection::build_batch_receipt(
        &request,
        &[
            tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                chunk_id: chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(chunk_digest.clone()),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(output_digest.clone()),
            },
        ],
    )
    .expect("added projection receipt");
    PreparedVectorGenerationV1 {
        embedding_key: embedding_key.clone(),
        request,
        receipt,
        vectors: vec![ProjectedChunkVectorV1 {
            projection_key,
            source_generation: to_generation.clone(),
            source_manifest_digest,
            chunk_id: chunk_id.clone(),
            chunk_digest: chunk_digest.clone(),
            values,
            output_digest,
        }],
        tombstones: vec![],
    }
}

async fn open_project_database(
    temporary: &tempfile::TempDir,
    operation: &'static str,
) -> (Database, DatabaseAuthority) {
    let path = temporary.path().join("project.db");
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(&path, operation).expect("authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("database");
    (database, authority)
}

async fn state_document(database: &Database) -> String {
    let mut rows = database
        .engine_conn()
        .query(
            "SELECT state_json FROM semantic_vector_generation_state_v1 WHERE singleton = 1",
            (),
        )
        .await
        .expect("state document");
    let row = rows.next().await.expect("state row").expect("state row");
    row.get::<String>(0).expect("state json")
}

async fn payload_row_count(database: &Database) -> i64 {
    let mut rows = database
        .engine_conn()
        .query("SELECT COUNT(*) FROM semantic_vector_payload_v1", ())
        .await
        .expect("payload count");
    let row = rows.next().await.expect("payload count row").expect("row");
    row.get::<i64>(0).expect("count")
}

fn insert_generation(
    store: &mut FakeVectorGenerationStoreV1,
    generation: PublishedVectorGenerationV1,
) -> VectorGenerationIdV1 {
    let generation_id = generation.generation_id().clone();
    intern_generation_vectors(
        &store.physical_vector_pool,
        &mut store.published,
        &generation,
    )
    .expect("intern generation vectors");
    store
        .published
        .generations
        .insert(generation_id.clone(), generation);
    generation_id
}
