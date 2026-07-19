//! Observation-authority, projection-alias, and cursor-receipt
//! merge/rollback consolidation tests.

use super::*;

#[tokio::test]
async fn observation_authority_merge_is_lossless_idempotent_and_replayable() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let first = migration_observation(0, 10, "receipt.migration.first", "message-migration-1");
    let second = migration_observation(10, 20, "receipt.migration.second", "message-migration-2");

    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(&target, first.clone(), None).await;
    assert_eq!(project_all_migration_observations(&target).await, 1);
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(&source, first, None).await;
    persist_migration_observation(&source, second, Some(migration_cursor(10))).await;
    assert_eq!(project_all_migration_observations(&source).await, 2);
    source.checkpoint().await;
    source.close();

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

    assert_observation_authority(&target_path).await;
    assert_pending_projection_replay(&target_path).await;

    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();

    assert_observation_authority(&target_path).await;
    assert_pending_projection_replay(&target_path).await;

    let merged = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    assert_eq!(project_all_migration_observations(&merged).await, 2);
    let checkpoint = GlobalDbObservationStore::new(&merged)
        .projection_checkpoint()
        .await
        .unwrap();
    assert_eq!(checkpoint.last_sequence(), 2);
    merged.close();
    assert_eq!(
        sqlite::count_rows(&target_path, "observation_projection_provenance")
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlite::count_rows(&target_path, "projection_queue")
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn observation_projection_remap_survives_drain_and_rebuild_to_zero() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
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

    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(&target, target_observation, None).await;
    assert_eq!(project_all_migration_observations(&target).await, 1);
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(&source, source_observation, None).await;
    assert_eq!(project_all_migration_observations(&source).await, 1);
    source.checkpoint().await;
    source.close();

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
    assert_projection_alias(&target_path, &source_observation_id, remapped_message_id).await;
    assert_eq!(
        sqlite::count_rows(&target_path, "projection_queue")
            .await
            .unwrap(),
        2
    );

    let merged = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    assert_eq!(project_all_migration_observations(&merged).await, 2);
    let rebuilt = GlobalDbObservationStore::new(&merged)
        .rebuild_projection(0)
        .await
        .unwrap();
    assert!(rebuilt.is_complete());
    assert_eq!(
        sqlite::count_rows(&target_path, "observation_projection_provenance")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlite::count_rows(&target_path, "session_messages")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlite::count_rows(&target_path, "observation_projection_aliases")
            .await
            .unwrap(),
        1
    );
    assert_eq!(project_all_migration_observations(&merged).await, 2);
    merged.close();
    assert_projection_output(&target_path, &source_observation_id, remapped_message_id).await;
}

#[tokio::test]
async fn shared_projection_owner_and_newer_source_owner_remain_lossless() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
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

    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(&target, shared.clone(), None).await;
    assert_eq!(project_all_migration_observations(&target).await, 1);
    set_migration_cursor(&target, session_id, 18, 0).await;
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(&source, shared, None).await;
    assert_eq!(project_all_migration_observations(&source).await, 1);
    persist_migration_observation(
        &source,
        newer,
        Some(migration_cursor_generation_for(session_id, 17, 10)),
    )
    .await;
    assert_eq!(project_all_migration_observations(&source).await, 1);
    assert_projection_ownership(&source_path, message_id, 1, 1).await;
    source.checkpoint().await;
    source.close();

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
    let merged = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    sqlite::verify_projection_plan_for_test(
        merged.conn(),
        &source_path,
        &target_input_path,
        "proj_source",
    )
    .await
    .unwrap();
    assert_shared_projection_predrain(
        &target_path,
        &shared_id,
        &newer_id,
        message_id,
        remapped_message_id,
    )
    .await;
    assert_eq!(project_all_migration_observations(&merged).await, 2);
    assert_message_text(&target_path, message_id, "older target body").await;
    assert_message_text(&target_path, remapped_message_id, "newer source body").await;
    assert_no_orphaned_projection_provenance(&target_path).await;

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
        merged.conn(),
        &source_path,
        &target_input_path,
        "proj_source",
    )
    .await
    .unwrap();
    assert_shared_projection_predrain(
        &target_path,
        &shared_id,
        &newer_id,
        message_id,
        remapped_message_id,
    )
    .await;
    assert_eq!(project_all_migration_observations(&merged).await, 2);
    assert_message_text(&target_path, message_id, "older target body").await;
    assert_message_text(&target_path, remapped_message_id, "newer source body").await;
    assert_no_orphaned_projection_provenance(&target_path).await;

    let rebuilt = GlobalDbObservationStore::new(&merged)
        .rebuild_projection(0)
        .await
        .unwrap();
    assert!(rebuilt.is_complete());
    assert_message_absent(&target_path, message_id).await;
    assert_message_absent(&target_path, remapped_message_id).await;
    assert_eq!(
        sqlite::count_rows(&target_path, "observation_projection_provenance")
            .await
            .unwrap(),
        0
    );
    assert_eq!(project_all_migration_observations(&merged).await, 2);
    merged.close();
    assert_message_text(&target_path, message_id, "older target body").await;
    assert_message_text(&target_path, remapped_message_id, "newer source body").await;
    assert_no_orphaned_projection_provenance(&target_path).await;
}

#[tokio::test]
async fn pending_target_observation_does_not_suppress_source_projection_claim() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let observation = migration_observation_for(
        "session.migration.pending-target",
        "receipt.migration.pending-target",
        "pending-target-message",
        "pending target body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(&target, observation.clone(), None).await;
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(&source, observation, None).await;
    assert_eq!(project_all_migration_observations(&source).await, 1);
    insert_projection_alias(
        &source,
        &observation_id,
        "consolidated/fixture/pending-target-message",
    )
    .await;
    source.checkpoint().await;
    source.close();

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
        &target_path,
        &observation_id,
        "consolidated/fixture/pending-target-message",
    )
    .await;
    assert_message_absent(&target_path, "pending-target-message").await;
    assert_message_absent(&target_path, "consolidated/fixture/pending-target-message").await;

    let merged = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    assert_eq!(project_all_migration_observations(&merged).await, 1);
    merged.close();
    assert_projection_output(
        &target_path,
        &observation_id,
        "consolidated/fixture/pending-target-message",
    )
    .await;
    assert_no_orphaned_projection_provenance(&target_path).await;
}

#[tokio::test]
async fn another_projector_claim_does_not_suppress_source_projection_claim() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let observation = migration_observation_for(
        "session.migration.second-projector",
        "receipt.migration.second-projector",
        "second-projector-message",
        "second projector body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(&target, observation.clone(), None).await;
    assert_eq!(project_all_migration_observations(&target).await, 1);
    target
        .writer_connection()
        .await
        .unwrap()
        .execute(
            "UPDATE observation_projection_provenance
             SET projector_version='test-projector-v2'",
            (),
        )
        .await
        .unwrap();
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(&source, observation, None).await;
    assert_eq!(project_all_migration_observations(&source).await, 1);
    insert_projection_alias(
        &source,
        &observation_id,
        "consolidated/fixture/second-projector-message",
    )
    .await;
    source.checkpoint().await;
    source.close();

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
        &target_path,
        &observation_id,
        "consolidated/fixture/second-projector-message",
    )
    .await;
    assert_message_text(
        &target_path,
        "second-projector-message",
        "second projector body",
    )
    .await;
    assert_message_absent(
        &target_path,
        "consolidated/fixture/second-projector-message",
    )
    .await;

    let merged = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    assert_eq!(project_all_migration_observations(&merged).await, 1);
    merged.close();
    assert_message_text(
        &target_path,
        "consolidated/fixture/second-projector-message",
        "second projector body",
    )
    .await;
    assert_no_orphaned_projection_provenance(&target_path).await;
}

#[tokio::test]
async fn observation_authority_collision_fails_before_session_merge_mutation() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let receipt_id = "receipt.migration.preflight-collision";
    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(
        &target,
        migration_observation_for(
            "session.migration.preflight-target",
            receipt_id,
            "preflight-target-message",
            "target receipt payload",
        ),
        None,
    )
    .await;
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(
        &source,
        migration_observation_for(
            "session.migration.preflight-source",
            receipt_id,
            "preflight-source-message",
            "source receipt payload",
        ),
        None,
    )
    .await;
    source.checkpoint().await;
    source.close();

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    let before = (
        sqlite::count_rows(&target_path, "sanitization_receipts")
            .await
            .unwrap(),
        sqlite::count_rows(&target_path, "observations")
            .await
            .unwrap(),
        sqlite::count_rows(&target_path, "source_cursors")
            .await
            .unwrap(),
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
            sqlite::count_rows(&target_path, "sanitization_receipts")
                .await
                .unwrap(),
            sqlite::count_rows(&target_path, "observations")
                .await
                .unwrap(),
            sqlite::count_rows(&target_path, "source_cursors")
                .await
                .unwrap(),
        ),
        before
    );
}

#[tokio::test]
async fn typed_duplicate_authority_repairs_noncanonical_target_json() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let observation = migration_observation_for(
        "session.migration.typed-duplicate",
        "receipt.migration.typed-duplicate",
        "typed-duplicate-message",
        "typed duplicate body",
    );
    for path in [&target_path, &source_path] {
        let db = GlobalDb::open_at_without_structured_backfill(path)
            .await
            .unwrap();
        persist_migration_observation(&db, observation.clone(), None).await;
        db.checkpoint().await;
        db.close();
    }

    let raw = libsql::Builder::new_local(&target_path)
        .build()
        .await
        .unwrap();
    let conn = raw.connect().unwrap();
    conn.execute_batch(
        "DROP TRIGGER observations_immutable_update;
         DROP TRIGGER observations_immutable_delete;
         DROP TRIGGER sanitization_receipts_immutable_update_v1;
         DROP TRIGGER sanitization_receipts_immutable_delete_v1;",
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
    conn.execute(
        "UPDATE sanitization_receipts SET receipt_json=?1",
        libsql::params![noncanonical_receipt],
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE observations SET observation_json=?1, committed_cursor_json=?2",
        libsql::params![noncanonical_observation, noncanonical_cursor],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw);

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

    let raw = libsql::Builder::new_local(&target_path)
        .build()
        .await
        .unwrap();
    let conn = raw.connect().unwrap();
    let mut rows = conn
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
    assert_eq!(row.get::<String>(0).unwrap(), canonical_receipt);
    assert_eq!(row.get::<String>(1).unwrap(), canonical_observation);
    assert_eq!(row.get::<String>(2).unwrap(), canonical_cursor);
}

#[tokio::test]
async fn source_cursor_advance_receipts_merge_losslessly_and_idempotently() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let source_json = serde_json::to_string(&migration_source()).unwrap();
    let scope_json = serde_json::to_string(&ObservationScopeV1::Profile).unwrap();
    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    target
        .writer_connection()
        .await
        .unwrap()
        .execute(
            "INSERT INTO source_cursor_advances(
                 source_json, scope_json, coverage_json, reason
             ) VALUES (?1, ?2, ?3, 'blank_frame')",
            libsql::params![
                source_json.as_str(),
                scope_json.as_str(),
                migration_coverage_json(0, 5)
            ],
        )
        .await
        .unwrap();
    target.checkpoint().await;
    target.close();
    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
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
        .writer_connection()
        .await
        .unwrap()
        .execute(
            "INSERT INTO sanitization_receipts(
                 receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES ('receipt.cursor.consolidation',
                       'sanitizer.migration-test.v1', '', ?1)",
            libsql::params![receipt_json.as_str()],
        )
        .await
        .unwrap();
    source
        .writer_connection()
        .await
        .unwrap()
        .execute(
            "INSERT INTO source_cursor_advances(
                 source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES (?1, ?2, ?3, 'sanitizer_rejected',
                       'receipt.cursor.consolidation')",
            libsql::params![
                source_json.as_str(),
                scope_json.as_str(),
                migration_coverage_json(5, 10)
            ],
        )
        .await
        .unwrap();
    source.checkpoint().await;
    source.close();

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
        sqlite::count_rows(&target_path, "source_cursor_advances")
            .await
            .unwrap(),
        2
    );
    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    let mut rows = target
        .conn()
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
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let source_json = serde_json::to_string(&migration_source()).unwrap();
    let scope_json = serde_json::to_string(&ObservationScopeV1::Profile).unwrap();
    for (path, reason) in [
        (&target_path, "blank_frame"),
        (&source_path, "out_of_scope"),
    ] {
        let db = GlobalDb::open_at_without_structured_backfill(path)
            .await
            .unwrap();
        db.writer_connection()
            .await
            .unwrap()
            .execute(
                "INSERT INTO source_cursor_advances(
                     source_json, scope_json, coverage_json, reason
                 ) VALUES (?1, ?2, ?3, ?4)",
                libsql::params![
                    source_json.as_str(),
                    scope_json.as_str(),
                    migration_coverage_json(0, 5),
                    reason
                ],
            )
            .await
            .unwrap();
        db.checkpoint().await;
        db.close();
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
        sqlite::count_rows(&target_path, "source_cursor_advances")
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn post_merge_projection_verification_rolls_back_transaction() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target_input_path = temp.path().join("target-input-sessions.db");
    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(
        &target,
        migration_observation_for(
            "session.migration.rollback",
            "receipt.migration.rollback",
            "rollback-message",
            "rollback body",
        ),
        None,
    )
    .await;
    assert_eq!(project_all_migration_observations(&target).await, 1);
    let mut rows = target
        .conn()
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
    target
        .writer_connection()
        .await
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
    target.close();
    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    source.checkpoint().await;
    source.close();

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

    let raw = libsql::Builder::new_local(&target_path)
        .build()
        .await
        .unwrap();
    let conn = raw.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT output_digest FROM observation_projection_provenance",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        original_digest
    );
}

#[tokio::test]
async fn malformed_target_only_cursor_fails_before_consolidation_mutation() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(
        &target,
        migration_observation(0, 10, "receipt.migration.target", "target-message"),
        None,
    )
    .await;
    let wrong_cursor = migration_cursor_for("session.migration.wrong", 10);
    let wrong_cursor_json = serde_json::to_string(&wrong_cursor).unwrap();
    target
        .writer_connection()
        .await
        .unwrap()
        .execute(
            "UPDATE source_cursors SET cursor_json=?1",
            libsql::params![wrong_cursor_json.clone()],
        )
        .await
        .unwrap();
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    source.checkpoint().await;
    source.close();
    let before = (
        sqlite::count_rows(&target_path, "sanitization_receipts")
            .await
            .unwrap(),
        sqlite::count_rows(&target_path, "observations")
            .await
            .unwrap(),
        sqlite::count_rows(&target_path, "source_cursors")
            .await
            .unwrap(),
    );
    let error = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source cursor authority keys disagree with cursor JSON")
    );
    assert_eq!(
        (
            sqlite::count_rows(&target_path, "sanitization_receipts")
                .await
                .unwrap(),
            sqlite::count_rows(&target_path, "observations")
                .await
                .unwrap(),
            sqlite::count_rows(&target_path, "source_cursors")
                .await
                .unwrap(),
        ),
        before
    );
    let raw = libsql::Builder::new_local(&target_path)
        .build()
        .await
        .unwrap();
    let conn = raw.connect().unwrap();
    let mut rows = conn
        .query("SELECT cursor_json FROM source_cursors", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        wrong_cursor_json
    );
}

#[tokio::test]
async fn projection_alias_represents_source_output_collision() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
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
    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    persist_migration_observation(&target, target_observation, None).await;
    assert_eq!(project_all_migration_observations(&target).await, 1);
    target.checkpoint().await;
    target.close();

    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(&source, source_observation, None).await;
    assert_eq!(project_all_migration_observations(&source).await, 1);
    insert_projection_alias(
        &source,
        &observation_id,
        "consolidated/fixture/alias-conflict-message",
    )
    .await;
    source.checkpoint().await;
    source.close();

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
        &target_path,
        &observation_id,
        "consolidated/fixture/alias-conflict-message",
    )
    .await;
    assert_message_text(&target_path, "alias-conflict-message", "target alias body").await;
    assert_message_absent(&target_path, "consolidated/fixture/alias-conflict-message").await;

    let merged = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    assert_eq!(project_all_migration_observations(&merged).await, 2);
    merged.close();
    assert_message_text(
        &target_path,
        "consolidated/fixture/alias-conflict-message",
        "source alias body",
    )
    .await;
    assert_no_orphaned_projection_provenance(&target_path).await;
}

#[tokio::test]
async fn inconsistent_projection_alias_fails_authority_preflight_without_target_mutation() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-sessions.db");
    let source_path = temp.path().join("source-sessions.db");
    let observation = migration_observation_for(
        "session.migration.invalid-alias",
        "receipt.migration.invalid-alias",
        "invalid-alias-message",
        "invalid alias body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    let target = GlobalDb::open_at_without_structured_backfill(&target_path)
        .await
        .unwrap();
    target.checkpoint().await;
    target.close();
    let source = GlobalDb::open_at_without_structured_backfill(&source_path)
        .await
        .unwrap();
    persist_migration_observation(&source, observation, None).await;
    assert_eq!(project_all_migration_observations(&source).await, 1);
    source
        .writer_connection()
        .await
        .unwrap()
        .execute(
            "INSERT INTO observation_projection_aliases(
                 projector_version, observation_id, output_provider, output_message_id
             ) VALUES (?1, ?2, 'claude', ?3)",
            libsql::params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id,
                "consolidated/fixture/invalid-alias-message"
            ],
        )
        .await
        .unwrap();
    source.checkpoint().await;
    source.close();

    let before = (
        sqlite::count_rows(&target_path, "observations")
            .await
            .unwrap(),
        sqlite::count_rows(&target_path, "observation_projection_provenance")
            .await
            .unwrap(),
        sqlite::count_rows(&target_path, "session_messages")
            .await
            .unwrap(),
    );
    let error = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap_err();
    let crate::errors::TraceDecayError::Database { message, operation } = error else {
        panic!("authority preflight must return a typed database error");
    };
    assert_eq!(operation, "ensure global database authority invariants");
    assert_eq!(
        message,
        "projection provenance disagrees with deterministic output"
    );
    assert_eq!(
        (
            sqlite::count_rows(&target_path, "observations")
                .await
                .unwrap(),
            sqlite::count_rows(&target_path, "observation_projection_provenance")
                .await
                .unwrap(),
            sqlite::count_rows(&target_path, "session_messages")
                .await
                .unwrap(),
        ),
        before
    );
}
