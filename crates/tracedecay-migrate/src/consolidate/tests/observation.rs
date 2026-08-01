//! Observation-authority, projection-alias, and cursor-receipt
//! merge/rollback consolidation tests.

use super::*;

struct ObservationDatabaseFixture {
    runtime: HostAdmissionTestRuntimeV1,
    path: PathBuf,
}

impl ObservationDatabaseFixture {
    async fn profile(profile_root: PathBuf) -> Self {
        let runtime = HostAdmissionTestRuntimeV1::profile(profile_root)
            .await
            .unwrap();
        let path = runtime
            .database_path(HostAdmissionScope::Profile)
            .unwrap()
            .to_path_buf();
        Self { runtime, path }
    }

    fn database(&self) -> &RegisteredGlobalDb {
        self.runtime
            .registered_database(HostAdmissionScope::Profile)
            .unwrap()
    }

    async fn checkpoint(&self) {
        self.database().checkpoint().await;
    }
}

async fn observation_backfill_watermark(db: &RegisteredGlobalDb, migration: &str) -> Option<i64> {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT backfilled_through FROM observation_backfill_watermarks
             WHERE migration = ?1",
            params![migration],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|row| row.get::<i64>(0).unwrap())
}

#[tokio::test]
async fn legacy_completed_backfills_resume_from_the_premerge_frontier() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let target_first = migration_observation_for(
        "session.legacy.target.first",
        "receipt.legacy-target-1",
        "legacy-target-1",
        "legacy target first",
    );
    let target_second = migration_observation_for(
        "session.legacy.target.second",
        "receipt.legacy-target-2",
        "legacy-target-2",
        "legacy target second",
    );

    Box::pin(persist_migration_observation(
        target.database(),
        target_first,
        None,
    ))
    .await;
    Box::pin(persist_migration_observation(
        target.database(),
        target_second,
        None,
    ))
    .await;
    Box::pin(persist_migration_observation(
        source.database(),
        migration_observation_for(
            "session.legacy.source.tail",
            "receipt.legacy-source-tail",
            "legacy-source-tail",
            "legacy source tail",
        ),
        None,
    ))
    .await;

    let writer = target.database().writer_connection().unwrap();
    writer
        .execute_batch(
            "DELETE FROM observation_backfill_watermarks;
             INSERT OR REPLACE INTO global_schema_migrations(migration)
             VALUES ('observation-retrieval-anchors-v2');
             INSERT OR REPLACE INTO global_schema_migrations(migration)
             VALUES ('observation-repository-provenance-v1');",
        )
        .await
        .unwrap();
    assert_eq!(
        observation_backfill_watermark(
            target.database(),
            crate::root_seam::global_db::observation::OBSERVATION_ANCHOR_SCHEMA_MIGRATION,
        )
        .await,
        None
    );
    assert_eq!(
        observation_backfill_watermark(
            target.database(),
            crate::root_seam::global_db::observation::OBSERVATION_PROVENANCE_SCHEMA_MIGRATION,
        )
        .await,
        None
    );
    target.checkpoint().await;
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "legacy_source",
        &offsets,
    )
    .await
    .unwrap();

    for migration in [
        crate::root_seam::global_db::observation::OBSERVATION_ANCHOR_SCHEMA_MIGRATION,
        crate::root_seam::global_db::observation::OBSERVATION_PROVENANCE_SCHEMA_MIGRATION,
    ] {
        assert_eq!(
            observation_backfill_watermark(target.database(), migration).await,
            Some(2),
            "{migration} must resume above the legacy target frontier"
        );
    }
}

#[tokio::test]
async fn observation_authority_merge_is_lossless_idempotent_and_replayable() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let first = migration_observation(0, 10, "receipt.migration.first", "message-migration-1");
    let second = migration_observation(10, 20, "receipt.migration.second", "message-migration-2");

    Box::pin(persist_migration_observation(
        target.database(),
        first.clone(),
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    target.checkpoint().await;

    Box::pin(persist_migration_observation(
        source.database(),
        first,
        None,
    ))
    .await;
    Box::pin(persist_migration_observation(
        source.database(),
        second,
        Some(migration_cursor(10)),
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        2
    );
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();

    assert_observation_authority(target.database()).await;
    assert_pending_projection_replay(target.database()).await;

    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();

    assert_observation_authority(target.database()).await;
    assert_pending_projection_replay(target.database()).await;

    assert_eq!(
        project_all_migration_observations(target.database()).await,
        2
    );
    let checkpoint = test_observation_store(target.database())
        .projection_checkpoint()
        .await
        .unwrap();
    assert_eq!(checkpoint.last_sequence(), 2);
    assert_eq!(
        registered_count_rows(target.database(), "observation_projection_provenance").await,
        2
    );
    assert_eq!(
        registered_count_rows(target.database(), "projection_queue").await,
        0
    );
}

#[tokio::test]
async fn observation_projection_remap_survives_drain_and_rebuild_to_zero() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let target_observation = migration_observation_for(
        "session.migration.target",
        "receipt.migration.target",
        "shared-projection-message",
        "target projection body",
    );
    let source_observation = migration_observation_for(
        "session.migration.source",
        "receipt.migration.source",
        "shared-projection-message",
        "source projection body",
    );
    let source_observation_id = source_observation.observation_id().as_str().to_owned();

    Box::pin(persist_migration_observation(
        target.database(),
        target_observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    target.checkpoint().await;

    Box::pin(persist_migration_observation(
        source.database(),
        source_observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        1
    );
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();

    let remapped_message_id = "consolidated/proj_source/shared-projection-message";
    assert_projection_alias(
        target.database(),
        &source_observation_id,
        remapped_message_id,
    )
    .await;
    assert_eq!(
        registered_count_rows(target.database(), "projection_queue").await,
        2
    );

    assert_eq!(
        project_all_migration_observations(target.database()).await,
        2
    );
    let rebuilt = test_observation_store(target.database())
        .rebuild_projection(0)
        .await
        .unwrap();
    assert!(rebuilt.is_complete());
    assert_eq!(
        registered_count_rows(target.database(), "observation_projection_provenance").await,
        0
    );
    assert_eq!(
        registered_count_rows(target.database(), "session_messages").await,
        0
    );
    assert_eq!(
        registered_count_rows(target.database(), "observation_projection_aliases").await,
        1
    );
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        2
    );
    assert_projection_output(
        target.database(),
        &source_observation_id,
        remapped_message_id,
    )
    .await;
}

#[tokio::test]
async fn shared_projection_owner_and_newer_source_owner_remain_lossless() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let session_id = "session.migration.shared-owner";
    let message_id = "shared-owner-message";
    let remapped_message_id = "consolidated/proj_source/shared-owner-message";
    let shared = migration_observation_generation(
        session_id,
        17,
        0,
        10,
        "receipt.migration.shared-owner",
        message_id,
        "older target body",
    );
    let newer = migration_observation_generation(
        session_id,
        18,
        0,
        10,
        "receipt.migration.newer-owner",
        message_id,
        "newer source body",
    );
    let shared_id = shared.observation_id().as_str().to_owned();
    let newer_id = newer.observation_id().as_str().to_owned();

    Box::pin(persist_migration_observation(
        target.database(),
        shared.clone(),
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    set_migration_cursor(target.database(), session_id, 18, 0).await;
    target.checkpoint().await;

    Box::pin(persist_migration_observation(
        source.database(),
        shared,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        1
    );
    Box::pin(persist_migration_observation(
        source.database(),
        newer,
        Some(migration_cursor_generation_for(session_id, 17, 10)),
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        1
    );
    assert_projection_ownership(source.database(), message_id, 1, 1).await;
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();
    sqlite::verify_projection_plan_for_test(
        target.database(),
        &source_path,
        &target_input_path,
        "proj_source",
    )
    .await
    .unwrap();
    assert_shared_projection_predrain(
        target.database(),
        &shared_id,
        &newer_id,
        message_id,
        remapped_message_id,
    )
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        2
    );
    assert_message_text(target.database(), message_id, "older target body").await;
    assert_message_text(target.database(), remapped_message_id, "newer source body").await;
    assert_no_orphaned_projection_provenance(target.database()).await;

    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();
    sqlite::verify_projection_plan_for_test(
        target.database(),
        &source_path,
        &target_input_path,
        "proj_source",
    )
    .await
    .unwrap();
    assert_shared_projection_predrain(
        target.database(),
        &shared_id,
        &newer_id,
        message_id,
        remapped_message_id,
    )
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        2
    );
    assert_message_text(target.database(), message_id, "older target body").await;
    assert_message_text(target.database(), remapped_message_id, "newer source body").await;
    assert_no_orphaned_projection_provenance(target.database()).await;

    let rebuilt = test_observation_store(target.database())
        .rebuild_projection(0)
        .await
        .unwrap();
    assert!(rebuilt.is_complete());
    assert_message_absent(target.database(), message_id).await;
    assert_message_absent(target.database(), remapped_message_id).await;
    assert_eq!(
        registered_count_rows(target.database(), "observation_projection_provenance").await,
        0
    );
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        2
    );
    assert_message_text(target.database(), message_id, "older target body").await;
    assert_message_text(target.database(), remapped_message_id, "newer source body").await;
    assert_no_orphaned_projection_provenance(target.database()).await;
}

#[tokio::test]
async fn pending_target_observation_does_not_suppress_source_projection_claim() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let observation = migration_observation_for(
        "session.migration.pending-target",
        "receipt.migration.pending-target",
        "pending-target-message",
        "pending target body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    Box::pin(persist_migration_observation(
        target.database(),
        observation.clone(),
        None,
    ))
    .await;
    target.checkpoint().await;

    Box::pin(persist_migration_observation(
        source.database(),
        observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        1
    );
    insert_projection_alias(
        source.database(),
        &observation_id,
        "consolidated/fixture/pending-target-message",
    )
    .await;
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();
    assert_projection_alias(
        target.database(),
        &observation_id,
        "consolidated/fixture/pending-target-message",
    )
    .await;
    assert_message_absent(target.database(), "pending-target-message").await;
    assert_message_absent(
        target.database(),
        "consolidated/fixture/pending-target-message",
    )
    .await;

    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    assert_projection_output(
        target.database(),
        &observation_id,
        "consolidated/fixture/pending-target-message",
    )
    .await;
    assert_no_orphaned_projection_provenance(target.database()).await;
}

#[tokio::test]
async fn another_projector_claim_does_not_suppress_source_projection_claim() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let observation = migration_observation_for(
        "session.migration.second-projector",
        "receipt.migration.second-projector",
        "second-projector-message",
        "second projector body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    Box::pin(persist_migration_observation(
        target.database(),
        observation.clone(),
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    target
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "UPDATE observation_projection_provenance
             SET projector_version='test-projector-v2'",
            (),
        )
        .await
        .unwrap();
    target.checkpoint().await;

    Box::pin(persist_migration_observation(
        source.database(),
        observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        1
    );
    insert_projection_alias(
        source.database(),
        &observation_id,
        "consolidated/fixture/second-projector-message",
    )
    .await;
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();
    assert_projection_alias(
        target.database(),
        &observation_id,
        "consolidated/fixture/second-projector-message",
    )
    .await;
    assert_message_text(
        target.database(),
        "second-projector-message",
        "second projector body",
    )
    .await;
    assert_message_absent(
        target.database(),
        "consolidated/fixture/second-projector-message",
    )
    .await;

    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    assert_message_text(
        target.database(),
        "consolidated/fixture/second-projector-message",
        "second projector body",
    )
    .await;
    assert_no_orphaned_projection_provenance(target.database()).await;
}

#[tokio::test]
async fn observation_authority_collision_fails_before_session_merge_mutation() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let receipt_id = "receipt.migration.preflight-collision";
    Box::pin(persist_migration_observation(
        target.database(),
        migration_observation_for(
            "session.migration.preflight-target",
            receipt_id,
            "preflight-target-message",
            "target receipt payload",
        ),
        None,
    ))
    .await;
    target.checkpoint().await;

    Box::pin(persist_migration_observation(
        source.database(),
        migration_observation_for(
            "session.migration.preflight-source",
            receipt_id,
            "preflight-source-message",
            "source receipt payload",
        ),
        None,
    ))
    .await;
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    let before = (
        registered_count_rows(target.database(), "sanitization_receipts").await,
        registered_count_rows(target.database(), "observations").await,
        registered_count_rows(target.database(), "source_cursors").await,
    );
    let error = sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("sanitization receipt identity collision")
    );
    assert_eq!(
        (
            registered_count_rows(target.database(), "sanitization_receipts").await,
            registered_count_rows(target.database(), "observations").await,
            registered_count_rows(target.database(), "source_cursors").await,
        ),
        before
    );
}

#[tokio::test]
async fn typed_duplicate_authority_repairs_noncanonical_target_json() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let observation = migration_observation_for(
        "session.migration.typed-duplicate",
        "receipt.migration.typed-duplicate",
        "typed-duplicate-message",
        "typed duplicate body",
    );
    for db in [target.database(), source.database()] {
        Box::pin(persist_migration_observation(db, observation.clone(), None)).await;
        db.checkpoint().await;
    }

    let writer = target.database().writer_connection().unwrap();
    crate::root_seam::global_db::schema_stages::begin_observation_authority_canonical_repair(
        &writer,
    )
    .await
    .unwrap();
    let canonical_receipt = serde_json::to_string(observation.receipt()).unwrap();
    let canonical_observation = serde_json::to_string(&observation).unwrap();
    let canonical_cursor = serde_json::to_string(&migration_cursor_for(
        observation.source().session_id().as_str(),
        observation.identity().position().end(),
    ))
    .unwrap();
    let noncanonical_receipt = serde_json::to_string_pretty(observation.receipt()).unwrap();
    let noncanonical_observation = serde_json::to_string_pretty(&observation).unwrap();
    let noncanonical_cursor = serde_json::to_string_pretty(
        &serde_json::from_str::<ClaudeSourceCursorV1>(&canonical_cursor).unwrap(),
    )
    .unwrap();
    writer
        .execute(
            "UPDATE sanitization_receipts SET receipt_json=?1",
            params![noncanonical_receipt],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observations SET observation_json=?1, committed_cursor_json=?2",
            params![noncanonical_observation, noncanonical_cursor],
        )
        .await
        .unwrap();
    crate::root_seam::global_db::schema_stages::finish_observation_authority_canonical_repair(
        &writer,
    )
    .await
    .unwrap();

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();

    let snapshot = target.database().read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT receipt.receipt_json, observation.observation_json,
                    observation.committed_cursor_json
             FROM observations AS observation
             JOIN sanitization_receipts AS receipt USING(receipt_id)",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let receipt_json = row.get::<String>(0).unwrap();
    let observation_json = row.get::<String>(1).unwrap();
    let cursor_json = row.get::<String>(2).unwrap();
    assert_eq!(receipt_json, canonical_receipt);
    assert_eq!(observation_json, canonical_observation);
    assert_eq!(cursor_json, canonical_cursor);
}

#[tokio::test]
async fn source_cursor_advance_receipts_merge_losslessly_and_idempotently() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let source_json = serde_json::to_string(&migration_source()).unwrap();
    let scope_json = serde_json::to_string(&ObservationScopeV1::Profile).unwrap();
    target
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO source_cursor_advances(
                 source_json, scope_json, coverage_json, reason
             ) VALUES (?1, ?2, ?3, 'blank_frame')",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                migration_coverage_json(0, 5)
            ],
        )
        .await
        .unwrap();
    target.checkpoint().await;
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.cursor.consolidation").unwrap(),
            ComponentVersion::new("sanitizer.migration-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Rejected,
        SensitivityV1::Sensitive,
        None,
    )
    .unwrap();
    let receipt_json = serde_json::to_string(&receipt).unwrap();
    source
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO sanitization_receipts(
                 receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES ('receipt.cursor.consolidation',
                       'sanitizer.migration-test.v1', '', ?1)",
            params![receipt_json.as_str()],
        )
        .await
        .unwrap();
    source
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO source_cursor_advances(
                 source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES (?1, ?2, ?3, 'sanitizer_rejected',
                       'receipt.cursor.consolidation')",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                migration_coverage_json(5, 10)
            ],
        )
        .await
        .unwrap();
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    for _ in 0..2 {
        sqlite::merge_sessions(
            &target_path,
            &source_path,
            &target_input_path,
            "proj_source",
            &offsets,
        )
        .await
        .unwrap();
    }
    assert_eq!(
        registered_count_rows(target.database(), "source_cursor_advances").await,
        2
    );
    let snapshot = target.database().read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT receipt_json FROM sanitization_receipts
             WHERE receipt_id = 'receipt.cursor.consolidation'",
            (),
        )
        .await
        .unwrap();
    let stored: SanitizationReceiptV1 = serde_json::from_str(
        &rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored, receipt);
}

#[tokio::test]
async fn source_cursor_advance_identity_collision_rolls_back_merge() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let source_json = serde_json::to_string(&migration_source()).unwrap();
    let scope_json = serde_json::to_string(&ObservationScopeV1::Profile).unwrap();
    for (db, reason) in [
        (target.database(), "blank_frame"),
        (source.database(), "out_of_scope"),
    ] {
        db.writer_connection()
            .unwrap()
            .execute(
                "INSERT INTO source_cursor_advances(
                     source_json, scope_json, coverage_json, reason
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    source_json.as_str(),
                    scope_json.as_str(),
                    migration_coverage_json(0, 5),
                    reason
                ],
            )
            .await
            .unwrap();
        db.checkpoint().await;
    }

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    let error = sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source cursor advance identity collision"),
        "{error}"
    );
    assert_eq!(
        registered_count_rows(target.database(), "source_cursor_advances").await,
        1
    );
}

#[tokio::test]
async fn post_merge_projection_verification_rolls_back_transaction() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    Box::pin(persist_migration_observation(
        target.database(),
        migration_observation_for(
            "session.migration.rollback",
            "receipt.migration.rollback",
            "rollback-message",
            "rollback body",
        ),
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    let snapshot = target.database().read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT output_digest FROM observation_projection_provenance",
            (),
        )
        .await
        .unwrap();
    let original_digest = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    drop(rows);
    drop(snapshot);
    target
        .database()
        .writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER corrupt_consolidated_projection_test
             AFTER INSERT ON observation_projection_provenance BEGIN
                 UPDATE observation_projection_provenance
                 SET output_digest = 'sha256:corrupt'
                 WHERE projector_version = NEW.projector_version
                   AND observation_id = NEW.observation_id;
             END;",
        )
        .await
        .unwrap();
    target.checkpoint().await;
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    let error = sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination projection provenance differs"),
        "{error}"
    );

    let snapshot = target.database().read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT output_digest FROM observation_projection_provenance",
            (),
        )
        .await
        .unwrap();
    let output_digest = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert_eq!(output_digest, original_digest);
}

#[tokio::test]
async fn malformed_target_only_cursor_fails_before_consolidation_mutation() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    Box::pin(persist_migration_observation(
        target.database(),
        migration_observation(0, 10, "receipt.migration.target", "target-message"),
        None,
    ))
    .await;
    let wrong_cursor = migration_cursor_for("session.migration.wrong", 10);
    let wrong_cursor_json = serde_json::to_string(&wrong_cursor).unwrap();
    target
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "UPDATE source_cursors SET cursor_json=?1",
            params![wrong_cursor_json.clone()],
        )
        .await
        .unwrap();
    target.checkpoint().await;

    source.checkpoint().await;
    let before = (
        registered_count_rows(target.database(), "sanitization_receipts").await,
        registered_count_rows(target.database(), "observations").await,
        registered_count_rows(target.database(), "source_cursors").await,
    );
    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    let error = sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cursor authority does not match its storage key")
    );
    assert_eq!(
        (
            registered_count_rows(target.database(), "sanitization_receipts").await,
            registered_count_rows(target.database(), "observations").await,
            registered_count_rows(target.database(), "source_cursors").await,
        ),
        before
    );
    let snapshot = target.database().read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query("SELECT cursor_json FROM source_cursors", ())
        .await
        .unwrap();
    let cursor_json = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert_eq!(cursor_json, wrong_cursor_json);
}

#[tokio::test]
async fn projection_alias_represents_source_output_collision() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input-sessions.db");
    let target_observation = migration_observation_for(
        "session.migration.alias-target",
        "receipt.migration.alias-conflict",
        "alias-conflict-message",
        "target alias body",
    );
    let source_observation = migration_observation_for(
        "session.migration.alias-source",
        "receipt.migration.alias-source",
        "alias-conflict-message",
        "source alias body",
    );
    let observation_id = source_observation.observation_id().as_str().to_owned();
    Box::pin(persist_migration_observation(
        target.database(),
        target_observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(target.database()).await,
        1
    );
    target.checkpoint().await;

    Box::pin(persist_migration_observation(
        source.database(),
        source_observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        1
    );
    insert_projection_alias(
        source.database(),
        &observation_id,
        "consolidated/fixture/alias-conflict-message",
    )
    .await;
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();
    assert_projection_alias(
        target.database(),
        &observation_id,
        "consolidated/fixture/alias-conflict-message",
    )
    .await;
    assert_message_text(
        target.database(),
        "alias-conflict-message",
        "target alias body",
    )
    .await;
    assert_message_absent(
        target.database(),
        "consolidated/fixture/alias-conflict-message",
    )
    .await;

    assert_eq!(
        project_all_migration_observations(target.database()).await,
        2
    );
    assert_message_text(
        target.database(),
        "consolidated/fixture/alias-conflict-message",
        "source alias body",
    )
    .await;
    assert_no_orphaned_projection_provenance(target.database()).await;
}

#[tokio::test]
async fn inconsistent_projection_alias_fails_authority_preflight_without_target_mutation() {
    let temp = TempDir::new().unwrap();
    let target = ObservationDatabaseFixture::profile(temp.path().join("target-profile")).await;
    let source = ObservationDatabaseFixture::profile(temp.path().join("source-profile")).await;
    let observation = migration_observation_for(
        "session.migration.invalid-alias",
        "receipt.migration.invalid-alias",
        "invalid-alias-message",
        "invalid alias body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    target.checkpoint().await;
    Box::pin(persist_migration_observation(
        source.database(),
        observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(source.database()).await,
        1
    );
    source
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO observation_projection_aliases(
                 projector_version, observation_id, output_provider, output_message_id
             ) VALUES (?1, ?2, 'claude', ?3)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id,
                "consolidated/fixture/invalid-alias-message"
            ],
        )
        .await
        .unwrap();
    source.checkpoint().await;

    let before = (
        registered_count_rows(target.database(), "observations").await,
        registered_count_rows(target.database(), "observation_projection_provenance").await,
        registered_count_rows(target.database(), "session_messages").await,
    );
    let snapshot = source.database().read_snapshot().await.unwrap();
    let error =
        crate::root_seam::global_db::schema_stages::validate_observation_authority_connection(
            &snapshot,
        )
        .await
        .unwrap_err();
    let tracedecay_runtime_core::errors::TraceDecayError::Database { message, operation } = error
    else {
        panic!("authority preflight must return a typed database error");
    };
    assert_eq!(operation, "ensure global database authority invariants");
    assert_eq!(
        message,
        "projection provenance disagrees with deterministic output"
    );
    assert_eq!(
        (
            registered_count_rows(target.database(), "observations").await,
            registered_count_rows(target.database(), "observation_projection_provenance").await,
            registered_count_rows(target.database(), "session_messages").await,
        ),
        before
    );
}
