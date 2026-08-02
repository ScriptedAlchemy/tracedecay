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
async fn legacy_inventory_never_deserializes_vectors_and_quarantines_only_unreadable_entries() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let path = temporary.path().join("project.db");
    crate::register_test_schema_installer();
    let authority =
        DatabaseAuthority::acquire_test(&path, "legacy vector migration").expect("authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("database");
    let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
        .await
        .expect("migration store");
    let readable = manifest_digest('a');
    let unreadable = manifest_digest('b');
    let source = "code-generation.legacy";
    let secret = "legacy-vector-secret";
    let generations = serde_json::Map::from_iter([
        (
            readable.as_str().to_owned(),
            serde_json::json!({
                "generation_id": readable.as_str(),
                "source_generation": source,
                "vectors": [secret]
            }),
        ),
        (unreadable.as_str().to_owned(), serde_json::json!(secret)),
    ]);
    let state = serde_json::json!({
        "staged": {},
        "published": {
            "generations": generations,
            "active_generation": readable.as_str(),
            "legacy_migration_receipts": {},
            "physical_vector_bindings": {}
        }
    })
    .to_string();
    database
        .execute_write_engine(
            "install unreadable legacy vector fixture",
            "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
            params![state],
        )
        .await
        .expect("legacy fixture");

    let inventory = store
        .read_legacy_inventory()
        .await
        .expect("identity-only inventory");
    assert_eq!(inventory.inventory.entries.len(), 2);
    assert!(matches!(
        &inventory.inventory.entries[0],
        LegacyVectorInventoryEntryV1::Readable { .. }
    ));
    assert!(matches!(
        &inventory.inventory.entries[1],
        LegacyVectorInventoryEntryV1::Unreadable { .. }
    ));
    let offline_sources = retained_readable_sources_from_read_only_database(&path)
        .expect("read-only source inventory");
    assert_eq!(
        offline_sources,
        BTreeSet::from([id(source)]),
        "offline retention planning must use exactly the readable source set"
    );
    let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(
        Vec::new(),
        |_| -> Result<
            StagedCanonicalVectorRebuildV1,
            tracedecay_semantic::legacy_migration::LegacyVectorMigrationErrorV1,
        > { unreachable!("no retained generations") },
    )
    .expect("empty production rebuilder");
    let transaction = prepare_legacy_vector_migration(
        &inventory,
        &mut rebuilder,
        &NeverCancelLegacyVectorMigrationV1,
    )
    .expect("migration transaction");
    store
        .replace_legacy_vectors_atomically(
            &inventory,
            FakeVectorGenerationStoreV1::new(),
            &transaction,
        )
        .await
        .expect("atomic replacement");

    assert_eq!(
        database
            .query_scalar_text(
                "inspect isolated legacy quarantine",
                "SELECT generation_json
                     FROM semantic_legacy_vector_quarantine_v1",
            )
            .await
            .expect("quarantine row"),
        serde_json::to_string(secret).expect("secret JSON")
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "prove readable legacy vectors were dropped",
                "SELECT COUNT(*)
                     FROM semantic_legacy_vector_quarantine_v1",
            )
            .await
            .expect("quarantine count"),
        1
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "prove legacy bytes left active state",
                "SELECT instr(state_json, 'legacy-vector-secret')
                     FROM semantic_vector_generation_state_v1
                     WHERE singleton = 1",
            )
            .await
            .expect("active state inspection"),
        0
    );
    let committed_state = database
        .query_scalar_text(
            "capture committed vector state",
            "SELECT state_json
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
        )
        .await
        .expect("committed state");
    assert_eq!(
        store
            .replace_legacy_vectors_atomically(
                &inventory,
                FakeVectorGenerationStoreV1::new(),
                &transaction,
            )
            .await,
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    );
    assert_eq!(
        database
            .query_scalar_text(
                "verify stale migration rollback",
                "SELECT state_json
                     FROM semantic_vector_generation_state_v1
                     WHERE singleton = 1",
            )
            .await
            .expect("state after stale migration"),
        committed_state
    );
    DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("replacement state is runtime-readable");
}

#[tokio::test]
async fn retained_canonical_rebuild_and_active_pointer_publish_together() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let path = temporary.path().join("project.db");
    crate::register_test_schema_installer();
    let authority =
        DatabaseAuthority::acquire_test(&path, "canonical vector rebuild").expect("authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("database");
    let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
        .await
        .expect("migration store");
    let legacy = manifest_digest('a');
    let source: CodeGenerationId = id("code-generation.retained");
    let legacy_generations = serde_json::Map::from_iter([(
        legacy.as_str().to_owned(),
        serde_json::json!({
            "generation_id": legacy.as_str(),
            "source_generation": source.as_str(),
            "vectors": "legacy-bytes-must-not-be-used"
        }),
    )]);
    let legacy_state = serde_json::json!({
        "staged": {},
        "published": {
            "generations": legacy_generations,
            "active_generation": legacy.as_str(),
            "legacy_migration_receipts": {},
            "physical_vector_bindings": {}
        }
    })
    .to_string();
    database
        .execute_write_engine(
            "install readable legacy vector fixture",
            "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
            params![legacy_state],
        )
        .await
        .expect("legacy fixture");
    let inventory = store
        .read_legacy_inventory()
        .await
        .expect("legacy inventory");
    let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
        source.clone(),
        vec![canonical_chunk("chunk.v1.retained", &source, 'd')],
    )
    .expect("retained canonical code");
    let mut replacement = FakeVectorGenerationStoreV1::new();
    let rebuilt = logical_generation(
        'c',
        admitted_embedding(),
        source.as_str(),
        '3',
        "chunk.v1.retained",
        'd',
        vec![0.5],
    );
    let rebuilt_id = insert_generation(&mut replacement, rebuilt);
    let rebuilt_for_callback = rebuilt_id.clone();
    let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(
        vec![retained],
        move |chunks: &CanonicalEligibleChunkSetV1| {
            Ok(StagedCanonicalVectorRebuildV1 {
                source_generation: chunks.source_generation().clone(),
                rebuilt_generation: rebuilt_for_callback.clone(),
                canonical_chunk_set_digest: chunks.digest().clone(),
            })
        },
    )
    .expect("production rebuilder");
    let transaction = prepare_legacy_vector_migration(
        &inventory,
        &mut rebuilder,
        &NeverCancelLegacyVectorMigrationV1,
    )
    .expect("canonical rebuild transaction");

    let receipt = store
        .replace_legacy_vectors_atomically(&inventory, replacement, &transaction)
        .await
        .expect("atomic canonical rebuild publication");
    assert_eq!(
        store
            .completed_legacy_migration_receipt()
            .await
            .expect("completed migration receipt"),
        Some(receipt)
    );

    let reopened = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("runtime store");
    assert_eq!(
        reopened
            .active_generation_id()
            .await
            .expect("active generation"),
        Some(rebuilt_id)
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "prove rebuild did not quarantine readable legacy bytes",
                "SELECT COUNT(*)
                     FROM sqlite_schema
                     WHERE type = 'table'
                       AND name = 'semantic_legacy_vector_quarantine_v1'",
            )
            .await
            .expect("quarantine schema count"),
        0
    );
}

#[tokio::test]
async fn request_read_ignores_corrupt_inactive_and_staged_generations() {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let path = temporary.path().join("project.db");
    crate::register_test_schema_installer();
    let authority =
        DatabaseAuthority::acquire_test(&path, "active vector request read").expect("authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("database");
    let _store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
        .await
        .expect("migration store");
    let embedding = admitted_embedding();
    let source: CodeGenerationId = id("code-generation.request-read");
    let source_manifest = manifest_digest('4');
    let active = logical_generation(
        'c',
        embedding.clone(),
        source.as_str(),
        '4',
        "chunk.v1.request-read",
        'd',
        vec![0.5],
    );
    let active_id = active.generation_id().clone();
    let mut state = FakeVectorGenerationStoreV1::new();
    insert_generation(&mut state, active);
    state.published.active_generation = Some(active_id.clone());
    install_test_vector_payloads(&database, VECTOR_PAYLOAD_TABLE_V1, &state).await;
    install_test_state_slices(&database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await;
    let mut state_json = serde_json::to_value(&state).expect("vector state JSON");
    state_json["published"]["generations"][manifest_digest('e').as_str()] =
        serde_json::json!("corrupt-inactive-vector-bytes");
    state_json["staged"] = serde_json::json!({
        "corrupt-build": "corrupt-staged-vector-bytes"
    });
    database
        .execute_write_engine(
            "install inactive corruption fixture",
            "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
            params![state_json.to_string()],
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
    assert!(
        DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .is_err(),
        "full-state decoding would observe unrelated corruption"
    );
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
