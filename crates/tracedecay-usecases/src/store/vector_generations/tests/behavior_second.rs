#[test]
fn cross_worktree_reuses_physical_bytes_without_reusing_logical_identity() {
    let embedding = admitted_embedding_for("privacy.reuse-regression-a", 7, "ort-test-rev-1");
    let first = logical_generation(
        'a',
        embedding.clone(),
        "code-generation.worktree-a",
        '1',
        "chunk.v1.worktree-a.alpha",
        'c',
        vec![0.25],
    );
    let second = logical_generation(
        'b',
        embedding.clone(),
        "code-generation.worktree-b",
        '2',
        "chunk.v1.worktree-b.alpha",
        'c',
        vec![0.25],
    );
    let first_chunk = first.vectors.keys().next().unwrap().clone();
    let second_chunk = second.vectors.keys().next().unwrap().clone();
    let first_generation = first.generation_id().clone();
    let second_generation = second.generation_id().clone();
    let mut first_store = FakeVectorGenerationStoreV1::new();
    let mut second_store = FakeVectorGenerationStoreV1::new();

    intern_generation_vectors(
        &first_store.physical_vector_pool,
        &mut first_store.published,
        &first,
    )
    .unwrap();
    first_store
        .published
        .generations
        .insert(first_generation.clone(), first.clone());
    first_store.published.active_generation = Some(first_generation.clone());
    intern_generation_vectors(
        &second_store.physical_vector_pool,
        &mut second_store.published,
        &second,
    )
    .unwrap();
    second_store
        .published
        .generations
        .insert(second_generation.clone(), second.clone());
    second_store.published.active_generation = Some(second_generation.clone());

    let first_values = first_store
        .physical_vector_values(&first_generation, &first_chunk)
        .unwrap();
    let second_values = second_store
        .physical_vector_values(&second_generation, &second_chunk)
        .unwrap();
    assert!(Arc::ptr_eq(&first_values, &second_values));
    assert_eq!(first_store.published.physical_vectors.len(), 1);
    assert_eq!(second_store.published.physical_vectors.len(), 1);
    assert_ne!(first_generation, second_generation);
    assert_ne!(first.source_generation(), second.source_generation());
    assert_ne!(first_chunk, second_chunk);
    assert_ne!(first.receipts(), second.receipts());
    assert_eq!(first_store.active_generation_id(), Some(&first_generation));
    assert_eq!(
        second_store.active_generation_id(),
        Some(&second_generation),
        "each worktree retains its own active pointer"
    );

    for (generation_digest, embedding_key) in [
        (
            'd',
            admitted_embedding_for("privacy.reuse-regression-b", 7, "ort-test-rev-1"),
        ),
        (
            'e',
            admitted_embedding_for("privacy.reuse-regression-a", 8, "ort-test-rev-1"),
        ),
        (
            'f',
            admitted_embedding_for("privacy.reuse-regression-a", 7, "ort-test-rev-2"),
        ),
    ] {
        let isolated = logical_generation(
            generation_digest,
            embedding_key,
            &format!("code-generation.isolated-{generation_digest}"),
            generation_digest,
            &format!("chunk.v1.isolated-{generation_digest}.alpha"),
            'c',
            vec![0.25],
        );
        intern_generation_vectors(
            &second_store.physical_vector_pool,
            &mut second_store.published,
            &isolated,
        )
        .unwrap();
        second_store
            .published
            .generations
            .insert(isolated.generation_id().clone(), isolated);
    }
    assert_eq!(
        second_store.published.physical_vectors.len(),
        4,
        "privacy domain, key epoch, and any projection-key input isolate physical bytes"
    );

    let edited_second = logical_generation(
        '9',
        embedding.clone(),
        "code-generation.worktree-b-edited",
        '9',
        "chunk.v1.worktree-b.alpha-edited",
        '9',
        vec![0.75],
    );
    let edited_generation = edited_second.generation_id().clone();
    let edited_chunk = edited_second.vectors.keys().next().unwrap().clone();
    intern_generation_vectors(
        &second_store.physical_vector_pool,
        &mut second_store.published,
        &edited_second,
    )
    .unwrap();
    second_store
        .published
        .generations
        .insert(edited_generation.clone(), edited_second);
    assert_eq!(second_store.published.physical_vectors.len(), 5);
    assert!(!Arc::ptr_eq(
        &second_values,
        &second_store
            .physical_vector_values(&edited_generation, &edited_chunk)
            .unwrap()
    ));
    assert!(Arc::ptr_eq(
        &first_values,
        &second_store
            .physical_vector_values(&second_generation, &second_chunk)
            .unwrap()
    ));
    assert!(Arc::ptr_eq(
        &first_values,
        &first_store
            .physical_vector_values(&first_generation, &first_chunk)
            .unwrap()
    ));
    assert_eq!(first_store.active_generation_id(), Some(&first_generation));
    assert_eq!(
        second_store.active_generation_id(),
        Some(&second_generation)
    );

    let conflicting = logical_generation(
        '8',
        embedding,
        "code-generation.worktree-c",
        '8',
        "chunk.v1.worktree-c.alpha",
        'c',
        vec![0.5],
    );
    assert_eq!(
        intern_generation_vectors(
            &second_store.physical_vector_pool,
            &mut second_store.published,
            &conflicting,
        ),
        Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict)
    );
}

#[test]
fn generation_identity_ignores_batch_execution_history() {
    let embedding_key = admitted_embedding();
    let projection_key = embedding_key.projection_key().clone();
    let source_generation = id::<CodeGenerationId>("code-generation.1");
    let source_manifest_digest = manifest_digest('b');
    let chunk_id = id::<CodeSearchChunkId>("chunk.v1.alpha");
    let plan = VectorGenerationPlanV1 {
        target_projection_key: projection_key.clone(),
        source_generation: source_generation.clone(),
        source_manifest_digest: source_manifest_digest.clone(),
        expected_chunk_ids: vec![chunk_id.clone()].into(),
        base_generation: None,
    };
    let vectors = BTreeMap::from([(
        chunk_id.clone(),
        ProjectedChunkVectorV1 {
            projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            chunk_id,
            chunk_digest: content_digest('c'),
            values: vec![0.25],
            // Identity tests compare digest bytes, not recomputed projector validity.
            output_digest: content_digest('d'),
        },
    )]);
    let tombstones = BTreeMap::new();

    let first = generation_identity_digest(&plan, &vectors, &tombstones)
        .expect("identity from vector content");
    let second = generation_identity_digest(&plan, &vectors, &tombstones)
        .expect("identity remains independent from receipt/checkpoint batching");

    assert_eq!(first, second);

    let checkpoint = VectorProjectionCheckpointV1 {
        target_projection_key: plan.target_projection_key.clone(),
        source_generation: plan.source_generation.clone(),
        source_manifest_digest: plan.source_manifest_digest.clone(),
        completed_batches: 1,
        last_request_digest: Some(manifest_digest('e')),
        last_publication_digest: Some(manifest_digest('f')),
    };
    let published = PublishedVectorGenerationV1 {
        generation_id: VectorGenerationIdV1::new(first.clone()),
        projection_key: plan.target_projection_key.clone(),
        source_generation: plan.source_generation.clone(),
        source_manifest_digest: plan.source_manifest_digest.clone(),
        base_generation: None,
        embedding_key,
        vectors: vectors.clone().into(),
        tombstones: vec![].into(),
        tombstone_digests: BTreeMap::new().into(),
        receipts: vec![].into(),
        checkpoint,
        manifest_digest: first,
    };
    let mut replayed = published.clone();
    replayed.checkpoint.completed_batches = 2;
    replayed.checkpoint.last_request_digest = Some(manifest_digest('0'));
    replayed.checkpoint.last_publication_digest = Some(manifest_digest('1'));

    assert_ne!(published.checkpoint, replayed.checkpoint);
    assert!(
        published.same_vector_content(&replayed),
        "execution checkpoint history does not redefine immutable vector content"
    );
    let mut rebuilt_from_another_base = published.clone();
    rebuilt_from_another_base.base_generation =
        Some(VectorGenerationIdV1::new(manifest_digest('9')));
    assert!(
        published.same_vector_content(&rebuilt_from_another_base),
        "execution lineage does not redefine identical immutable vector content"
    );

    let mut sealed_source = FakeVectorGenerationStoreV1::new();
    sealed_source
        .published
        .generations
        .insert(published.generation_id().clone(), published.clone());
    let sealed = seal_test_state(&mut sealed_source);
    let published = sealed_source
        .published
        .generations
        .values()
        .next()
        .expect("sealed generation")
        .clone();
    let encoded = serde_json::to_string(&published).expect("serialize published generation");
    assert!(
        !encoded.contains("\"values\""),
        "the state document must not carry inline float payloads"
    );
    assert!(
        !encoded.contains("\"chunk_digest\""),
        "the state document must not carry per-vector row metadata"
    );
    let mut decoded: PublishedVectorGenerationV1 =
        serde_json::from_str(&encoded).expect("deserialize published generation");
    assert!(
        decoded.vectors().is_empty(),
        "decoded rows resolve from externalized collection storage"
    );
    decoded
        .visit_external_slots(&mut |slot| {
            let Some(address) = slot.address().cloned() else {
                return Ok(());
            };
            slot.fill(sealed.get(&address).expect("sealed collection"))
        })
        .expect("fill externalized collections");
    for (chunk_id, vector) in decoded.vectors.elided_mut().iter_mut() {
        vector
            .values
            .clone_from(&published.vectors[chunk_id].values);
    }
    assert!(published.same_vector_content(&decoded));
    assert_eq!(decoded.tombstones(), published.tombstones());
    assert_eq!(decoded.tombstone_digests(), published.tombstone_digests());
    assert_eq!(decoded.base_generation(), published.base_generation());
    assert_eq!(decoded.embedding_key(), published.embedding_key());
}

#[test]
fn persisted_state_rejects_tombstone_vector_overlap_and_dangling_active() {
    let embedding_key = admitted_embedding();
    let projection_key = embedding_key.projection_key().clone();
    let chunk_id = id::<CodeSearchChunkId>("chunk.v1.alpha");
    let generation_id = VectorGenerationIdV1::new(manifest_digest('a'));
    let mut generation = PublishedVectorGenerationV1 {
        generation_id: generation_id.clone(),
        projection_key: projection_key.clone(),
        source_generation: id("code-generation.1"),
        source_manifest_digest: manifest_digest('b'),
        base_generation: None,
        embedding_key: embedding_key.clone(),
        vectors: BTreeMap::from([(
            chunk_id.clone(),
            ProjectedChunkVectorV1 {
                projection_key,
                source_generation: id("code-generation.1"),
                source_manifest_digest: manifest_digest('b'),
                chunk_id: chunk_id.clone(),
                chunk_digest: content_digest('c'),
                values: vec![1.0],
                output_digest: content_digest('d'),
            },
        )])
        .into(),
        tombstones: vec![chunk_id.clone()].into(),
        tombstone_digests: BTreeMap::from([(chunk_id, content_digest('c'))]).into(),
        receipts: vec![].into(),
        checkpoint: VectorProjectionCheckpointV1 {
            target_projection_key: embedding_key.projection_key().clone(),
            source_generation: id("code-generation.1"),
            source_manifest_digest: manifest_digest('b'),
            completed_batches: 1,
            last_request_digest: None,
            last_publication_digest: None,
        },
        manifest_digest: generation_id.as_digest().clone(),
    };
    assert!(generation.validate_persisted().is_err());

    generation.vectors.clear();
    generation.canonicalize_tombstones();
    let request_digest = manifest_digest('e');
    let mut deletion_batch = ProjectionBatchReceiptV1 {
        target_projection_key: generation.projection_key.clone(),
        request_digest: request_digest.clone(),
        source_generation: generation.source_generation.clone(),
        source_manifest_digest: generation.source_manifest_digest.clone(),
        receipts: vec![tracedecay_domain::CodeChunkProjectionReceiptV1 {
            projection_key: generation.projection_key.clone(),
            request_digest: request_digest.clone(),
            prior_generation: Some(id("code-generation.0")),
            source_generation: generation.source_generation.clone(),
            source_manifest_digest: generation.source_manifest_digest.clone(),
            chunk_id: generation.tombstones[0].clone(),
            prior_chunk_digest: generation
                .tombstone_digests
                .get(&generation.tombstones[0])
                .cloned(),
            current_chunk_digest: None,
            operation: ProjectionOperationV1::Deleted,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: None,
        }],
        reused_count: 0,
        publication_digest: manifest_digest('f'),
    };
    deletion_batch.publication_digest =
        expected_publication_digest(&deletion_batch).expect("deletion publication digest");
    generation.checkpoint.last_request_digest = Some(request_digest);
    generation.checkpoint.last_publication_digest = Some(deletion_batch.publication_digest.clone());
    *generation.receipts = vec![deletion_batch];
    generation.manifest_digest = generation_identity_digest(
        &VectorGenerationPlanV1 {
            target_projection_key: generation.projection_key.clone(),
            source_generation: generation.source_generation.clone(),
            source_manifest_digest: generation.source_manifest_digest.clone(),
            expected_chunk_ids: vec![].into(),
            base_generation: None,
        },
        &generation.vectors,
        &generation.tombstone_digests,
    )
    .expect("tombstone generation manifest");
    generation.generation_id = VectorGenerationIdV1::new(generation.manifest_digest.clone());
    assert!(generation.validate_persisted().is_ok());

    let mut state = FakeVectorGenerationStoreV1::default();
    state.published.active_generation = Some(VectorGenerationIdV1::new(manifest_digest('9')));
    assert!(validate_loaded_state(&state).is_err());
}

#[test]
fn persisted_generation_recomputes_immutable_manifest_content() {
    let mut generation = logical_generation(
        'a',
        admitted_embedding(),
        "code-generation.manifest-integrity",
        'b',
        "chunk.v1.manifest-integrity",
        'c',
        vec![0.25],
    );
    generation
        .validate_persisted()
        .expect("canonical generation");
    let vector = generation
        .vectors
        .values_mut()
        .next()
        .expect("fixture vector");
    vector.values = vec![0.75];
    vector.output_digest = tracedecay_semantic::projector::vector_output_digest(
        &vector.projection_key,
        &vector.chunk_id,
        &vector.chunk_digest,
        &vector.values,
    )
    .expect("tampered vector digest");
    generation.receipts[0].receipts[0].output_digest = Some(vector.output_digest.clone());
    generation.receipts[0].publication_digest =
        expected_publication_digest(&generation.receipts[0]).expect("tampered publication digest");
    generation.checkpoint.last_publication_digest =
        Some(generation.receipts[0].publication_digest.clone());

    assert!(
        generation.validate_persisted().is_err(),
        "self-consistent vector/receipt tampering must not retain the immutable generation id"
    );
}

/// The externalized store must produce exactly the identity the in-memory
/// state machine produces for the same inputs, must keep the float payload
/// out of the state document, and must let a restart resume a staged build.
#[tokio::test]
async fn row_per_vector_storage_preserves_identity_and_resumes_staged_builds() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let (database, _authority) = open_project_database(&temporary, "row per vector storage").await;
    let embedding = admitted_embedding();
    let source: CodeGenerationId = id("code-generation.row-per-vector");
    let chunk_id: CodeSearchChunkId = id("chunk.v1.row-per-vector");
    let chunk_digest = content_digest('a');
    let prepared = added_prepared(
        &embedding,
        &source,
        &chunk_id,
        &chunk_digest,
        vec![0.312_5_f32],
    );
    let plan = VectorGenerationPlanV1 {
        target_projection_key: embedding.projection_key().clone(),
        source_generation: source.clone(),
        source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
        expected_chunk_ids: vec![chunk_id.clone()].into(),
        base_generation: None,
    };

    // The oracle: the same plan and batch through the pure state machine.
    let mut oracle = FakeVectorGenerationStoreV1::new();
    let oracle_build = oracle
        .begin_generation(plan.clone())
        .expect("oracle build identity");
    oracle
        .commit_batch(&oracle_build, None, prepared.clone())
        .expect("oracle batch");
    let oracle_publication = oracle
        .publish_generation(&oracle_build, None)
        .expect("oracle publication");

    let store = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("open vector generation store");
    let build = store
        .begin_generation(plan)
        .await
        .expect("durable build identity");
    assert_eq!(build, oracle_build);
    let checkpoint = store
        .commit_batch(&build, None, prepared.clone())
        .await
        .expect("durable batch");
    assert_eq!(checkpoint.completed_batches, 1);

    let document = state_document(&database).await;
    assert!(
        !document.contains("\"values\""),
        "the state document must not carry inline float payloads"
    );
    assert_eq!(
        payload_row_count(&database).await,
        1,
        "the committed batch persists exactly its own vector row"
    );

    // Restart: a fresh handle over the same database resumes the staged
    // build and publishes the byte-identical generation identity.
    let restarted = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("reopen vector generation store");
    let publication = restarted
        .publish_generation(&build, None)
        .await
        .expect("publish resumed build");
    assert_eq!(publication.generation_id, oracle_publication.generation_id);
    assert_eq!(
        publication.manifest_digest,
        oracle_publication.manifest_digest
    );
    assert_eq!(publication.checkpoint, oracle_publication.checkpoint);

    let observed = restarted
        .active_generation()
        .await
        .expect("read active generation")
        .expect("active generation");
    let expected = oracle
        .generation(&oracle_publication.generation_id)
        .expect("oracle generation");
    assert_eq!(&observed, expected, "round trip restores the exact vectors");
    assert_eq!(
        observed.vectors()[&chunk_id].values,
        vec![0.312_5_f32],
        "float payloads survive the row encoding exactly"
    );
    assert_eq!(
        observed.receipts(),
        expected.receipts(),
        "receipts are unchanged by externalized payload storage"
    );

    let bounded = DatabaseVectorGenerationStoreV1::read_active_generation_for(
        &database,
        &embedding,
        &source,
        observed.source_manifest_digest(),
    )
    .await
    .expect("bounded active read")
    .expect("compatible active generation");
    assert_eq!(&bounded, expected);

    // Publication retires the staged batch copy, so its payload row is the
    // published one and nothing more.
    assert_eq!(payload_row_count(&database).await, 1);
}

/// A pre-migration state document carries every float inline. Opening the
/// store must move those floats to row-per-vector storage, drop them from
/// the document, and leave every identity untouched.
#[tokio::test]
async fn opening_a_legacy_inline_state_migrates_payloads_to_rows() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let (database, _authority) =
        open_project_database(&temporary, "legacy inline payload migration").await;
    let embedding = admitted_embedding();
    let source: CodeGenerationId = id("code-generation.legacy-inline");
    let generation = logical_generation(
        'c',
        embedding.clone(),
        source.as_str(),
        '4',
        "chunk.v1.legacy-inline",
        'd',
        vec![0.75_f32],
    );
    let generation_id = generation.generation_id().clone();
    let source_manifest_digest = generation.source_manifest_digest().clone();
    let expected = generation.clone();
    let mut state = FakeVectorGenerationStoreV1::new();
    insert_generation(&mut state, generation);
    state.published.active_generation = Some(generation_id.clone());

    // Re-inline both externalizations to reproduce the pre-migration
    // encoding: corpus-sized collections rendered in place, and every
    // float carried inside its vector row.
    let mut document = legacy_inline_document(&mut state);
    let vectors =
        document["published"]["generations"][generation_id.as_digest().as_str()]["vectors"]
            .as_object_mut()
            .expect("vector map");
    for vector in vectors.values_mut() {
        vector["values"] = serde_json::json!([0.75_f32]);
    }
    let legacy_document = document.to_string();
    assert!(legacy_document.contains("\"values\""));
    assert!(legacy_document.contains("\"chunk_digest\""));
    DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
        .await
        .expect("schema");
    database
        .execute_write_engine(
            "install legacy inline vector fixture",
            "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
            params![legacy_document],
        )
        .await
        .expect("install legacy fixture");
    assert_eq!(payload_row_count(&database).await, 0);

    let store = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("open migrates the legacy document");
    assert_eq!(payload_row_count(&database).await, 1);
    let document = state_document(&database).await;
    assert!(
        !document.contains("\"values\""),
        "migration drops the inline floats from the state document"
    );
    let observed = store
        .active_generation()
        .await
        .expect("read active generation")
        .expect("active generation");
    assert_eq!(observed.generation_id(), &generation_id);
    assert_eq!(&observed, &expected, "migration preserves the generation");

    // Re-opening a migrated store is a no-op rather than a second rewrite.
    let revision_before = state_revision(&database).await;
    DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("reopen migrated store");
    assert_eq!(state_revision(&database).await, revision_before);
    assert_eq!(
        DatabaseVectorGenerationStoreV1::read_active_generation_for(
            &database,
            &embedding,
            &source,
            &source_manifest_digest,
        )
        .await
        .expect("bounded active read")
        .expect("compatible active generation")
        .vectors()
        .values()
        .next()
        .expect("vector")
        .values,
        vec![0.75_f32]
    );
}

/// Retiring a generation must release the interner keys it introduced, or
/// the process-global pool grows for the lifetime of the daemon.
#[test]
fn physical_byte_pool_releases_keys_for_retired_generations() {
    let pool = PhysicalVectorBytePoolV1::default();
    pool.sweep_retired().expect("sweep");
    let baseline = pool.retained_entries();
    {
        let mut retained = Vec::new();
        for index in 0..64_u64 {
            let embedding = admitted_embedding_for("privacy.pool-scope", index, "ort-pool");
            let reuse_key = PhysicalVectorReuseKeyV1 {
                canonical_chunk_digest: content_digest('a'),
                projection_key: embedding.projection_key().clone(),
                admitted_embedding_key: embedding.clone(),
                privacy_domain: embedding.privacy_domain().clone(),
                privacy_key_epoch: embedding.privacy_key_epoch(),
            };
            retained.push(pool.intern(&reuse_key, &[0.5_f32]).expect("intern"));
        }
        assert_eq!(
            pool.retained_entries(),
            baseline + 64,
            "live generations retain their interned identities"
        );
        pool.sweep_retired().expect("sweep with live handles");
        assert_eq!(
            pool.retained_entries(),
            baseline + 64,
            "a sweep never drops a live entry"
        );
    }
    pool.sweep_retired().expect("sweep after retire");
    assert_eq!(
        pool.retained_entries(),
        baseline,
        "retiring the generations releases every key they interned"
    );
}
