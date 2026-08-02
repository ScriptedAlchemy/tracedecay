#[test]
fn batch_watermark_and_base_generation_must_match_the_projection_request() {
    let embedding = admitted_embedding();
    let base = logical_generation(
        'a',
        embedding.clone(),
        "code-generation.base",
        'b',
        "chunk.v1.base",
        'c',
        vec![0.25],
    );
    let chunk_id = base.vectors.keys().next().expect("base chunk").clone();
    let chunk_digest = base
        .vectors
        .get(&chunk_id)
        .expect("base vector")
        .chunk_digest
        .clone();
    let base_id = base.generation_id().clone();
    let mut store = FakeVectorGenerationStoreV1::new();
    insert_generation(&mut store, base);
    let foreign_source = id("code-generation.foreign");
    let target_source = id("code-generation.target");
    let prepared = reused_prepared(
        &embedding,
        &foreign_source,
        &target_source,
        &chunk_id,
        &chunk_digest,
    );
    let build = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: embedding.projection_key().clone(),
            source_generation: target_source.clone(),
            source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
            expected_chunk_ids: vec![chunk_id.clone()].into(),
            base_generation: Some(base_id.clone()),
        })
        .expect("staged build");
    assert_eq!(
        store.commit_batch(&build, None, prepared.clone()),
        Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)
    );

    let mismatched_manifest = manifest_digest('f');
    let mismatched_build = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: embedding.projection_key().clone(),
            source_generation: target_source,
            source_manifest_digest: mismatched_manifest,
            expected_chunk_ids: vec![chunk_id].into(),
            base_generation: Some(base_id),
        })
        .expect("mismatched-watermark build");
    assert_eq!(
        store.commit_batch(&mismatched_build, None, prepared),
        Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch)
    );
}

#[test]
fn successful_publication_consumes_the_staged_build() {
    let embedding = admitted_embedding();
    let base = logical_generation(
        'a',
        embedding.clone(),
        "code-generation.base",
        'b',
        "chunk.v1.base",
        'c',
        vec![0.25],
    );
    let chunk_id = base.vectors.keys().next().expect("base chunk").clone();
    let chunk_digest = base
        .vectors
        .get(&chunk_id)
        .expect("base vector")
        .chunk_digest
        .clone();
    let base_source = base.source_generation().clone();
    let base_id = base.generation_id().clone();
    let target_source = id("code-generation.target");
    let prepared = reused_prepared(
        &embedding,
        &base_source,
        &target_source,
        &chunk_id,
        &chunk_digest,
    );
    let mut store = FakeVectorGenerationStoreV1::new();
    insert_generation(&mut store, base);
    store.published.active_generation = Some(base_id.clone());
    let build = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: embedding.projection_key().clone(),
            source_generation: target_source,
            source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
            expected_chunk_ids: vec![chunk_id].into(),
            base_generation: Some(base_id.clone()),
        })
        .expect("staged build");
    store
        .commit_batch(&build, None, prepared)
        .expect("complete reused batch");
    let publication = store
        .publish_generation(&build, Some(&base_id))
        .expect("atomic publication");

    assert!(!store.staged.contains_key(&build));
    assert_eq!(
        store.active_generation_id(),
        Some(&publication.generation_id)
    );
    store
        .active_generation()
        .expect("current generation")
        .validate_persisted()
        .expect("current generation is complete");
}

#[tokio::test]
async fn opening_exact_empty_singleton_binds_the_active_pointer_and_drops_the_singleton() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let (database, _authority) =
        open_project_database(&temporary, "empty vector singleton cutover").await;
    database
        .execute_write_batch(
            "install empty vector singleton fixture",
            VECTOR_GENERATION_STATE_SCHEMA_V1,
        )
        .await
        .expect("legacy singleton schema");
    database
        .execute_write_engine(
            "install empty vector singleton fixture",
            "INSERT INTO semantic_vector_generation_state_v1 (
                singleton, revision, state_json
             ) VALUES (1, 0, ?1)",
            params![
                r#"{"staged":{},"published":{"generations":{},"active_generation":null,"legacy_migration_receipts":{},"physical_vector_bindings":{}}}"#
            ],
        )
        .await
        .expect("legacy singleton row");

    DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("exact empty singleton migrates");

    assert_eq!(
        database
            .query_scalar_i64(
                "prove singleton removed",
                "SELECT COUNT(*)
                 FROM sqlite_schema
                 WHERE type = 'table'
                   AND name = 'semantic_vector_generation_state_v1'",
            )
            .await
            .expect("singleton schema count"),
        0
    );
    let mut rows = database
        .engine_conn()
        .query(
            "SELECT revision, shard_id_json, generation_id
             FROM semantic_vector_active_generation_v1
             WHERE singleton = 1",
            (),
        )
        .await
        .expect("active pointer");
    let row = rows
        .next()
        .await
        .expect("active pointer row")
        .expect("active pointer row");
    assert_eq!(row.get::<i64>(0).expect("revision"), 0);
    let bound: tracedecay_store::StoreShardIdV1 =
        serde_json::from_str(&row.get::<String>(1).expect("shard identity"))
            .expect("canonical shard identity");
    assert_eq!(&bound, &database.retained_runtime().binding().shard_id);
    assert_eq!(
        row.get::<Option<String>>(2).expect("generation identity"),
        None
    );
}

#[tokio::test]
async fn opening_nonempty_singleton_fails_typed_without_partial_cutover() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let (database, _authority) =
        open_project_database(&temporary, "nonempty vector singleton refusal").await;
    database
        .execute_write_batch(
            "install nonempty vector singleton fixture",
            VECTOR_GENERATION_STATE_SCHEMA_V1,
        )
        .await
        .expect("legacy singleton schema");
    database
        .execute_write_engine(
            "install nonempty vector singleton fixture",
            "INSERT INTO semantic_vector_generation_state_v1 (
                singleton, revision, state_json
             ) VALUES (1, 0, ?1)",
            params![
                r#"{"staged":{},"published":{"generations":{},"active_generation":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","legacy_migration_receipts":{},"physical_vector_bindings":{}}}"#
            ],
        )
        .await
        .expect("legacy singleton row");

    assert!(matches!(
        DatabaseVectorGenerationStoreV1::open(&database).await,
        Err(VectorGenerationStoreErrorV1::NonemptyLegacySingleton)
    ));
    assert_eq!(
        database
            .query_scalar_i64(
                "prove singleton retained after refusal",
                "SELECT COUNT(*)
                 FROM sqlite_schema
                 WHERE type = 'table'
                   AND name = 'semantic_vector_generation_state_v1'",
            )
            .await
            .expect("singleton schema count"),
        1
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "prove cutover rolled back",
                "SELECT COUNT(*)
                 FROM sqlite_schema
                 WHERE type = 'table'
                   AND name = 'semantic_vector_active_generation_v1'",
            )
            .await
            .expect("active schema count"),
        0
    );
}

#[tokio::test]
async fn reopening_rejects_an_active_pointer_bound_to_another_shard() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let (database, _authority) =
        open_project_database(&temporary, "vector active pointer identity").await;
    DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("initial vector store");
    database
        .execute_write_engine(
            "corrupt vector active pointer identity",
            "UPDATE semantic_vector_active_generation_v1
             SET shard_id_json = '{}'
             WHERE singleton = 1",
            (),
        )
        .await
        .expect("foreign shard fixture");

    assert!(matches!(
        DatabaseVectorGenerationStoreV1::open(&database).await,
        Err(VectorGenerationStoreErrorV1::ShardIdentityMismatch)
    ));
}


#[tokio::test]
async fn request_read_ignores_corrupt_inactive_and_staged_generations() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let (database, _authority) =
        open_project_database(&temporary, "active vector request read").await;
    let store = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("vector store");
    let embedding = admitted_embedding();
    let source: CodeGenerationId = id("code-generation.request-read");
    let chunk_id: CodeSearchChunkId = id("chunk.v1.request-read");
    let prepared = added_prepared(
        &embedding,
        &source,
        &chunk_id,
        &content_digest('d'),
        vec![0.5],
    );
    let source_manifest = prepared.request.changes.manifest_digest.clone();
    let build = store
        .begin_generation(VectorGenerationPlanV1 {
            target_projection_key: embedding.projection_key().clone(),
            source_generation: source.clone(),
            source_manifest_digest: source_manifest.clone(),
            expected_chunk_ids: vec![chunk_id].into(),
            base_generation: None,
        })
        .await
        .expect("staged generation");
    store
        .commit_batch(&build, None, prepared)
        .await
        .expect("batch");
    let active_id = store
        .publish_generation(&build, None)
        .await
        .expect("publication")
        .generation_id;
    database
        .execute_write_batch(
            "install inactive corruption fixture",
            &format!(
                "INSERT INTO semantic_vector_generation_v1 (
                    build_id, revision, lifecycle, generation_id, record_json
                 ) VALUES (
                    '{}', 0, 'published', '{}', 'corrupt-inactive-vector-bytes'
                 );
                 INSERT INTO semantic_vector_generation_v1 (
                    build_id, revision, lifecycle, generation_id, record_json
                 ) VALUES (
                    '{}', 0, 'staged', NULL, 'corrupt-staged-vector-bytes'
                 );",
                manifest_digest('e').as_str(),
                manifest_digest('e').as_str(),
                manifest_digest('f').as_str(),
            ),
        )
        .await
        .expect("corrupt inactive fixture");

    let observed = DatabaseVectorGenerationStoreV1::read_active_generation_for(
        &database,
        &embedding,
        &source,
        &source_manifest,
    )
    .await
    .expect("bounded active read")
    .expect("compatible active generation");
    assert_eq!(observed.generation_id(), &active_id);
    assert!(
        DatabaseVectorGenerationStoreV1::read_active_generation_snapshot_for(
            &database,
            &embedding,
            &source,
            &manifest_digest('5'),
        )
        .await
        .expect("wrong-manifest active read")
        .is_none(),
        "an active generation with the wrong source manifest must be denied"
    );
    DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("open does not scan unrelated generation rows");
}

#[tokio::test]
async fn native_evaluation_state_is_sqlite_backed_and_never_becomes_authoritative() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let path = temporary.path().join("project.db");
    crate::register_test_schema_installer();
    let authority =
        DatabaseAuthority::acquire_test(&path, "native semantic evaluation").expect("authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("database");

    let evaluation =
        DatabaseVectorEvaluationStoreV1::open(&database, "semantic-native-evaluation:test")
            .await
            .expect("SQLite-backed evaluation store");
    assert_eq!(
        evaluation
            .active_generation_id()
            .await
            .expect("evaluation active generation"),
        None
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "inspect native evaluation row",
                "SELECT COUNT(*) FROM semantic_vector_evaluation_state_v1",
            )
            .await
            .expect("evaluation row count"),
        1
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "prove native evaluation did not create authoritative state",
                "SELECT COUNT(*)
                     FROM sqlite_schema
                     WHERE type = 'table'
                       AND name = 'semantic_vector_generation_state_v1'",
            )
            .await
            .expect("authoritative schema count"),
        0
    );

    evaluation.close().await.expect("remove evaluation row");
    assert_eq!(
        database
            .query_scalar_i64(
                "verify native evaluation cleanup",
                "SELECT COUNT(*) FROM semantic_vector_evaluation_state_v1",
            )
            .await
            .expect("evaluation row count after cleanup"),
        0
    );
}

#[test]
fn active_pointer_cas_fault_restart_and_semantic_off_are_atomic() {
    let embedding = admitted_embedding();
    let first = logical_generation(
        'a',
        embedding.clone(),
        "code-generation.atomic-a",
        '1',
        "chunk.v1.atomic-a",
        'a',
        vec![0.25],
    );
    let second = logical_generation(
        'b',
        embedding,
        "code-generation.atomic-b",
        '2',
        "chunk.v1.atomic-b",
        'b',
        vec![0.75],
    );
    let mut store = FakeVectorGenerationStoreV1::new();
    let first_id = insert_generation(&mut store, first);
    let second_id = insert_generation(&mut store, second);
    store.published.active_generation = Some(first_id.clone());

    assert_eq!(
        store.activate_generation(&second_id, None),
        Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
    );
    assert_eq!(store.active_generation_id(), Some(&first_id));

    store.fail_before_publication_swap_once();
    assert_eq!(
        store.activate_generation(&second_id, Some(&first_id)),
        Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure)
    );
    assert_eq!(store.active_generation_id(), Some(&first_id));

    store
        .activate_generation(&second_id, Some(&first_id))
        .expect("activate replacement generation");
    assert_eq!(
        store.deactivate_generation(Some(&first_id)),
        Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
    );
    assert_eq!(store.active_generation_id(), Some(&second_id));
    // The state document carries neither float payloads nor corpus-sized
    // collections; a restart resolves both from their own tables, which
    // this round trip stands in for.
    let mut restarted = restart_round_trip(&mut store);
    restarted
        .ensure_physical_reuse_index()
        .expect("rebuild physical reuse index");
    validate_loaded_state(&restarted).expect("validate restarted vector state");
    assert_eq!(restarted.active_generation_id(), Some(&second_id));

    restarted.fail_before_publication_swap_once();
    assert_eq!(
        restarted.deactivate_generation(Some(&second_id)),
        Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure)
    );
    assert_eq!(restarted.active_generation_id(), Some(&second_id));

    restarted
        .deactivate_generation(Some(&second_id))
        .expect("disable semantic generation");
    assert_eq!(restarted.active_generation_id(), None);
    assert!(
        restarted.generation(&second_id).is_some(),
        "semantic-off retains the immutable generation for rollback"
    );
    restarted
        .activate_generation(&second_id, None)
        .expect("restore exact retained generation");
    assert_eq!(restarted.active_generation_id(), Some(&second_id));
}
