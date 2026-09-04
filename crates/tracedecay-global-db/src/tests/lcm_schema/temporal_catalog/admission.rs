use super::*;

async fn persisted_column_names(db_path: &Path, table: &str) -> Vec<String> {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_xinfo(?1) ORDER BY cid",
            params![table],
        )
        .await
        .unwrap();
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        columns.push(row.get::<String>(0).unwrap());
    }
    columns
}

async fn schema_object_exists(db_path: &Path, object_type: &str, name: &str) -> bool {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2",
            params![object_type, name],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

async fn schema_object_sql(db_path: &Path, object_type: &str, name: &str) -> String {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2",
            params![object_type, name],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
}

async fn suspend_schema_triggers(db_path: &Path) -> Vec<String> {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT name, sql FROM sqlite_master
             WHERE type = 'trigger' AND sql IS NOT NULL ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut triggers = Vec::new();
    let mut names = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        names.push(row.get::<String>(0).unwrap());
        triggers.push(row.get::<String>(1).unwrap());
    }
    drop(rows);
    for name in names {
        conn.execute(&format!("DROP TRIGGER \"{name}\""), ())
            .await
            .unwrap();
    }
    triggers
}

async fn restore_schema_triggers(db_path: &Path, triggers: &[String]) {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    for sql in triggers {
        conn.execute_batch(sql).await.unwrap();
    }
}

async fn convert_final_temporal_schema_to_released_v3(db_path: &Path) {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'session_refresh_progress_insert_guard_v1'",
            (),
        )
        .await
        .unwrap();
    let current_guard = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    drop(rows);
    let current_accounting = "AND NEW.committed_records = receipt.committed_item_count";
    let released_accounting = "AND NEW.committed_records =
                                receipt.occurrence_count
                                + receipt.copy_count
                                + receipt.assertion_count";
    assert_eq!(current_guard.matches(current_accounting).count(), 1);
    let released_guard = current_guard.replacen(current_accounting, released_accounting, 1);

    conn.execute_batch(
        "DROP TRIGGER session_refresh_progress_insert_guard_v1;
         DROP INDEX idx_session_relation_receipts_recovery_due;
         ALTER TABLE session_temporal_projection_receipts DROP COLUMN batch_item_count;
         ALTER TABLE session_temporal_projection_receipts DROP COLUMN committed_item_count;
         ALTER TABLE session_temporal_projection_receipts DROP COLUMN committed_copy_count;
         ALTER TABLE session_relation_receipts DROP COLUMN recovery_state;
         ALTER TABLE session_relation_receipts DROP COLUMN recovery_failure_code;
         ALTER TABLE session_relation_receipts DROP COLUMN recovery_failure_count;
         ALTER TABLE session_relation_receipts DROP COLUMN recovery_next_attempt_at;",
    )
    .await
    .unwrap();
    conn.execute_batch(&released_guard).await.unwrap();
    conn.execute(
        "UPDATE session_temporal_schema_migrations
         SET version = 3, applied_at = 100
         WHERE name = 'session-temporal'",
        (),
    )
    .await
    .unwrap();
}

async fn insert_released_v3_projection_receipts(db_path: &Path, second_counts: (i64, i64, i64)) {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('released-v3', 1, 'building', '{}', 100);
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'released-v3', 1, 0,
            'sha256:1000000000000000000000000000000000000000000000000000000000000000',
            '{}', 5, 5,
            2, 'sha256:1100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1200000000000000000000000000000000000000000000000000000000000000',
            1, 'sha256:1300000000000000000000000000000000000000000000000000000000000000',
            1, 'sha256:1400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1700000000000000000000000000000000000000000000000000000000000000',
            101
         );",
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'released-v3', 1, 1,
            'sha256:2000000000000000000000000000000000000000000000000000000000000000',
            '{}', 10, 10,
            ?1, 'sha256:2100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2200000000000000000000000000000000000000000000000000000000000000',
            ?2, 'sha256:2300000000000000000000000000000000000000000000000000000000000000',
            ?3, 'sha256:2400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2700000000000000000000000000000000000000000000000000000000000000',
            102
         )",
        params![second_counts.0, second_counts.1, second_counts.2],
    )
    .await
    .unwrap();
}

async fn insert_seeded_active_released_v3_refresh_receipts(
    db_path: &Path,
    second_candidate_counts: (i64, i64, i64),
) {
    let second_candidate_total =
        second_candidate_counts.0 + second_candidate_counts.1 + second_candidate_counts.2;
    let triggers = suspend_schema_triggers(db_path).await;
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at,
            ready_at, activated_at
         ) VALUES (
            'released-v3', 1, 'active',
            '{\"active_generation\":1,\"source_frontier\":5,\"projection_frontier\":5,\"summary_frontier\":0,\"cursor_key\":null}',
            90, 91, 92
         );
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES (
            'released-v3', 2, 'building',
            '{\"active_generation\":1,\"source_frontier\":5,\"projection_frontier\":5,\"summary_frontier\":0,\"cursor_key\":null}',
            100
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'released-v3', 1, 0,
            'sha256:0100000000000000000000000000000000000000000000000000000000000000',
            '{\"active_generation\":1,\"source_frontier\":5,\"projection_frontier\":5,\"summary_frontier\":0,\"cursor_key\":null}',
            5, 5,
            3, 'sha256:0200000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:0300000000000000000000000000000000000000000000000000000000000000',
            1, 'sha256:0400000000000000000000000000000000000000000000000000000000000000',
            1, 'sha256:0500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:0600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:0700000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:0800000000000000000000000000000000000000000000000000000000000000',
            99
         );
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at, terminal_at
         ) VALUES (
            'released-v3', 'refresh-v3-baseline',
            'sha256:a000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":5,\"committed_through\":0}',
            'complete', 90, 99, 99
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'released-v3', 'refresh-v3-baseline', 'session_store', 0, 5,
            'session-temporal-projector.v1',
            'sha256:a100000000000000000000000000000000000000000000000000000000000000',
            1,
            '{\"active_generation\":1,\"source_frontier\":5,\"projection_frontier\":5,\"summary_frontier\":0,\"cursor_key\":null}',
            'sha256:a000000000000000000000000000000000000000000000000000000000000000',
            90
         );
         INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'released-v3', 'refresh-v3-baseline', 0,
            '{\"observed_through\":5,\"committed_through\":5}',
            '{\"visible\":5,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 5, 95
         );
         INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('released-v3', 'refresh-v3-baseline', 0, 1, 0);
         INSERT INTO session_refresh_receipts (
            session_id, operation_id, terminal_state, frontier_json, coverage_json,
            failure_code, terminal_at
         ) VALUES (
            'released-v3', 'refresh-v3-baseline', 'complete',
            '{\"observed_through\":5,\"committed_through\":5}',
            '{\"visible\":5,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            NULL, 99
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'released-v3', 2, 0,
            'sha256:1000000000000000000000000000000000000000000000000000000000000000',
            '{\"active_generation\":1,\"source_frontier\":5,\"projection_frontier\":5,\"summary_frontier\":0,\"cursor_key\":null}',
            7, 7,
            6, 'sha256:1100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1200000000000000000000000000000000000000000000000000000000000000',
            1, 'sha256:1300000000000000000000000000000000000000000000000000000000000000',
            2, 'sha256:1400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1700000000000000000000000000000000000000000000000000000000000000',
            101
         );
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'released-v3', 'refresh-v3',
            'sha256:9000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":10,\"committed_through\":5}',
            'running', 100, 100
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'released-v3', 'refresh-v3', 'session_store', 5, 10,
            'session-temporal-projector.v1',
            'sha256:9100000000000000000000000000000000000000000000000000000000000000',
            2,
            '{\"active_generation\":1,\"source_frontier\":5,\"projection_frontier\":5,\"summary_frontier\":0,\"cursor_key\":null}',
            'sha256:9000000000000000000000000000000000000000000000000000000000000000',
            100
         );
         INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'released-v3', 'refresh-v3', 0,
            '{\"observed_through\":10,\"committed_through\":7}',
            '{\"visible\":9,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 9, 101
         );
         INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('released-v3', 'refresh-v3', 0, 2, 0);",
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'released-v3', 2, 1,
            'sha256:2000000000000000000000000000000000000000000000000000000000000000',
            '{\"active_generation\":1,\"source_frontier\":5,\"projection_frontier\":5,\"summary_frontier\":0,\"cursor_key\":null}',
            10, 10,
            ?1, 'sha256:2100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2200000000000000000000000000000000000000000000000000000000000000',
            ?2, 'sha256:2300000000000000000000000000000000000000000000000000000000000000',
            ?3, 'sha256:2400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2700000000000000000000000000000000000000000000000000000000000000',
            102
         )",
        params![
            second_candidate_counts.0,
            second_candidate_counts.1,
            second_candidate_counts.2
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'released-v3', 'refresh-v3', 1,
            '{\"observed_through\":10,\"committed_through\":10}',
            json_object('visible', ?1, 'hidden', 0, 'unknown', 0, 'redacted', 0),
            2, ?1, 102
         )",
        params![second_candidate_total],
    )
    .await
    .unwrap();
    conn.execute_batch(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('released-v3', 'refresh-v3', 1, 2, 1);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    restore_schema_triggers(db_path, &triggers).await;
}

async fn projection_receipt_progress_counts(db_path: &Path) -> Vec<(i64, i64, i64, i64)> {
    let raw_db = TestConnection::open(db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT batch_ordinal, batch_item_count, committed_item_count,
                    committed_copy_count
             FROM session_temporal_projection_receipts
             WHERE session_id = 'released-v3' AND generation = 2
             ORDER BY batch_ordinal",
            (),
        )
        .await
        .unwrap();
    let mut counts = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        counts.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
            row.get(3).unwrap(),
        ));
    }
    counts
}

#[tokio::test]
async fn released_v3_temporal_receipts_migrate_to_v4_with_exact_progress_counts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal schema");
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    insert_seeded_active_released_v3_refresh_receipts(&db_path, (8, 2, 2)).await;

    let reopened = open_global_db(&db_path)
        .await
        .expect("the exact published v3 temporal shape should migrate atomically");
    drop(reopened);

    assert_eq!(temporal_schema_version(&db_path).await, 4);
    assert_eq!(
        projection_receipt_progress_counts(&db_path).await,
        [(0, 4, 9, 1), (1, 3, 12, 2)]
    );
    assert!(
        normalized_trigger_sql(&db_path, "session_refresh_progress_insert_guard_v1")
            .await
            .contains("new.committed_records=receipt.committed_item_count"),
        "migration must install the v4 refresh accounting guard before commit"
    );
    assert!(
        schema_object_exists(
            &db_path,
            "trigger",
            "session_temporal_projection_receipts_immutable_update_v1"
        )
        .await,
        "migration must restore projection receipt immutability before commit"
    );
}

#[tokio::test]
async fn released_v3_changed_check_constraint_is_refused_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.unwrap();
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    let released_sql =
        schema_object_sql(&db_path, "table", "session_temporal_projection_receipts").await;
    let drifted_sql =
        released_sql.replacen("CHECK(batch_ordinal >= 0)", "CHECK(batch_ordinal >= -1)", 1);
    assert_ne!(drifted_sql, released_sql);
    let triggers = suspend_schema_triggers(&db_path).await;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch("DROP TABLE session_temporal_projection_receipts;")
        .await
        .unwrap();
    conn.execute_batch(&drifted_sql).await.unwrap();
    drop(conn);
    drop(raw_db);
    restore_schema_triggers(&db_path, &triggers).await;
    let persisted_drifted_sql =
        schema_object_sql(&db_path, "table", "session_temporal_projection_receipts").await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("changed released-v3 CHECK constraint must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert_eq!(
        schema_object_sql(&db_path, "table", "session_temporal_projection_receipts").await,
        persisted_drifted_sql
    );
}

#[tokio::test]
async fn released_v3_extra_temporal_trigger_is_refused_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.unwrap();
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE TRIGGER branch_local_temporal_trigger
         BEFORE INSERT ON session_temporal_projection_receipts BEGIN SELECT 1; END;",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("extra released-v3 temporal trigger must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert!(schema_object_exists(&db_path, "trigger", "branch_local_temporal_trigger").await);
}

#[tokio::test]
async fn unbound_released_v3_receipts_are_refused_without_fabricated_batch_counts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.unwrap();
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    insert_released_v3_projection_receipts(&db_path, (5, 3, 2)).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("unbound released-v3 receipts must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert!(
        !persisted_column_names(&db_path, "session_temporal_projection_receipts")
            .await
            .iter()
            .any(|column| column == "batch_item_count")
    );
}

#[tokio::test]
async fn valid_watermarks_unbound_released_v3_receipts_refuse_duplicate_batch_semantics() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.unwrap();
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    insert_released_v3_projection_receipts(&db_path, (5, 3, 2)).await;
    let triggers = suspend_schema_triggers(&db_path).await;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "UPDATE session_temporal_generations
         SET frozen_watermarks_json =
             '{\"active_generation\":1,\"source_frontier\":10,\"projection_frontier\":10,\"summary_frontier\":0,\"cursor_key\":null}'
         WHERE session_id = 'released-v3' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_projection_receipts
         SET frozen_watermarks_json =
             '{\"active_generation\":1,\"source_frontier\":10,\"projection_frontier\":10,\"summary_frontier\":0,\"cursor_key\":null}'
         WHERE session_id = 'released-v3' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    restore_schema_triggers(&db_path, &triggers).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("unbound v3 duplicate-batch semantics must not be fabricated"),
        Err(error) => error,
    };
    let (authority, reason) = error.reset_required_context().unwrap();
    assert_eq!(authority, "session temporal");
    assert!(reason.contains("unbound or ambiguous"));
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert!(
        !persisted_column_names(&db_path, "session_temporal_projection_receipts")
            .await
            .iter()
            .any(|column| column == "batch_item_count")
    );
}

#[tokio::test]
async fn ambiguous_released_v3_refresh_progress_rolls_back_without_batch_counts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.unwrap();
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    insert_seeded_active_released_v3_refresh_receipts(&db_path, (8, 2, 2)).await;
    let triggers = suspend_schema_triggers(&db_path).await;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "UPDATE session_refresh_progress
         SET committed_records = 11,
             coverage_json = '{\"visible\":11,\"hidden\":0,\"unknown\":0,\"redacted\":0}'
         WHERE session_id = 'released-v3' AND operation_id = 'refresh-v3'
           AND progress_ordinal = 1",
        (),
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    restore_schema_triggers(&db_path, &triggers).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("ambiguous released-v3 refresh progress must be refused"),
        Err(error) => error,
    };
    let (authority, reason) = error.reset_required_context().unwrap();
    assert_eq!(authority, "session temporal");
    assert!(reason.contains("unbound or ambiguous"));
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert!(
        !persisted_column_names(&db_path, "session_temporal_projection_receipts")
            .await
            .iter()
            .any(|column| column == "batch_item_count")
    );
}

#[tokio::test]
async fn non_monotonic_released_v3_receipts_roll_back_the_v4_migration() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal schema");
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    insert_seeded_active_released_v3_refresh_receipts(&db_path, (1, 1, 1)).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("non-monotonic released-v3 receipts must not migrate"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("invalid released-v3 progress must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("non-monotonic"),
        "unexpected reason: {reason}"
    );
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert_eq!(
        persisted_column_names(&db_path, "session_temporal_projection_receipts").await,
        [
            "session_id",
            "generation",
            "batch_ordinal",
            "batch_digest",
            "frozen_watermarks_json",
            "source_through",
            "projection_through",
            "occurrence_count",
            "occurrence_digest",
            "dimension_count",
            "dimension_digest",
            "copy_count",
            "copy_digest",
            "assertion_count",
            "assertion_digest",
            "supersession_count",
            "supersession_digest",
            "current_count",
            "current_digest",
            "fts_count",
            "fts_digest",
            "committed_at",
        ]
    );
}

#[tokio::test]
async fn drifted_released_v3_temporal_shape_is_refused_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal schema");
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "ALTER TABLE session_temporal_projection_receipts
         ADD COLUMN branch_local_count INTEGER;",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("drifted v3 receipt storage must not migrate"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert_eq!(
        persisted_column_names(&db_path, "session_temporal_projection_receipts")
            .await
            .last()
            .map(String::as_str),
        Some("branch_local_count")
    );
}

#[tokio::test]
async fn drifted_released_v3_temporal_trigger_is_refused_without_repair() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal schema");
    drop(db);
    convert_final_temporal_schema_to_released_v3(&db_path).await;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "DROP TRIGGER session_refresh_progress_insert_guard_v1;
         CREATE TRIGGER session_refresh_progress_insert_guard_v1
         BEFORE INSERT ON session_refresh_progress BEGIN SELECT 1; END;",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let drifted_guard =
        normalized_trigger_sql(&db_path, "session_refresh_progress_insert_guard_v1").await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("drifted v3 temporal triggers must not be repaired and migrated"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );
    assert_eq!(temporal_schema_version(&db_path).await, 3);
    assert_eq!(
        normalized_trigger_sql(&db_path, "session_refresh_progress_insert_guard_v1").await,
        drifted_guard,
        "refused v3 trigger drift must remain untouched"
    );
    assert!(
        !persisted_column_names(&db_path, "session_temporal_projection_receipts")
            .await
            .iter()
            .any(|column| column == "batch_item_count")
    );
}

#[tokio::test]
async fn temporal_schema_accepts_only_fresh_or_exact_final_stores() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch("CREATE TABLE session_temporal_generations (wrong_column TEXT);")
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a partial temporal schema must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("partial temporal schema must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("final schema"),
        "unexpected reason: {reason}"
    );
    assert!(
        !table_exists(&db_path, "session_temporal_schema_migrations").await,
        "a rejected temporal schema must not gain a version marker"
    );
    assert!(
        !table_exists(&db_path, "session_summary_nodes").await,
        "a rejected temporal schema must not gain authority tables"
    );

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute("DROP TABLE session_temporal_generations", ())
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    let db = open_global_db(&db_path)
        .await
        .expect("a fresh store should receive the final temporal schema");
    drop(db);
    let initial_catalog = temporal_schema_object_catalog(&db_path).await;
    let initial_version = temporal_schema_version(&db_path).await;

    let restart_path = tmp.path().join(".tracedecay").join("restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = open_global_db(&restart_path)
        .await
        .expect("idempotent temporal reopen should succeed");
    drop(reopened);
    assert_eq!(
        temporal_schema_version(&restart_path).await,
        initial_version
    );
    assert_eq!(
        temporal_schema_object_catalog(&restart_path).await,
        initial_catalog
    );
}

#[tokio::test]
async fn temporal_schema_rejects_extra_marker_rows_without_mutating_them() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal marker");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "UPDATE session_temporal_schema_migrations
         SET applied_at = 90
         WHERE name = 'session-temporal';
         INSERT INTO session_temporal_schema_migrations (name, version, applied_at)
         VALUES ('unexpected-temporal-marker', 4, 91);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let before_catalog = temporal_schema_object_catalog(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a current marker plus extra rows must require reset"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT name, version, applied_at
             FROM session_temporal_schema_migrations
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut marker_rows = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        marker_rows.push((
            row.get::<String>(0).unwrap(),
            row.get::<i64>(1).unwrap(),
            row.get::<i64>(2).unwrap(),
        ));
    }
    assert_eq!(
        marker_rows,
        [
            ("session-temporal".to_string(), 4, 90),
            ("unexpected-temporal-marker".to_string(), 4, 91),
        ],
        "typed refusal must preserve every temporal marker row"
    );
    assert_eq!(
        temporal_schema_object_catalog(&db_path).await,
        before_catalog,
        "typed refusal must not rewrite the current schema"
    );
}

#[tokio::test]
async fn temporal_schema_malformed_marker_requires_reset_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE TABLE session_temporal_schema_migrations (
            name TEXT PRIMARY KEY,
            incompatible_version TEXT NOT NULL,
            applied_at INTEGER NOT NULL
         );
         INSERT INTO session_temporal_schema_migrations (
            name, incompatible_version, applied_at
         ) VALUES ('session-temporal', 'older-shape', 91);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a malformed temporal marker must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("a malformed temporal marker must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(reason.contains("version"), "unexpected reason: {reason}");
    assert_eq!(
        row_count(&db_path, "session_temporal_schema_migrations").await,
        1,
        "typed refusal must preserve malformed marker rows"
    );
    assert!(
        !table_exists(&db_path, "session_summary_nodes").await,
        "typed refusal must not install temporal authority tables"
    );
}

#[tokio::test]
async fn temporal_schema_lower_marker_requires_reset_without_repairing_guards() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "DROP TRIGGER session_refresh_progress_insert_guard_v1;
         CREATE TRIGGER session_refresh_progress_insert_guard_v1
         BEFORE INSERT ON session_refresh_progress BEGIN SELECT 1; END;
         UPDATE session_temporal_schema_migrations
         SET version = 2
         WHERE name = 'session-temporal';",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let stale_guard =
        normalized_trigger_sql(&db_path, "session_refresh_progress_insert_guard_v1").await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a lower temporal marker must not be upgraded"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("a lower marker must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(reason.contains("version 2"), "unexpected reason: {reason}");
    assert_eq!(temporal_schema_version(&db_path).await, 2);
    assert_eq!(
        normalized_trigger_sql(&db_path, "session_refresh_progress_insert_guard_v1").await,
        stale_guard,
        "rejected lower-version schema must not have its trigger repaired"
    );
}

#[tokio::test]
async fn temporal_schema_rejects_transition_storage_without_mutating_it() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh store should receive the final temporal schema");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_temporal_migration_receipts (
            branch_local_row INTEGER NOT NULL
         );",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let before_catalog = temporal_schema_object_catalog(&db_path).await;
    let before_version = temporal_schema_version(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("branch-local transition storage must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("transition storage must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("session_temporal_migration_receipts"),
        "unexpected reason: {reason}"
    );
    assert!(
        table_exists(&db_path, "session_temporal_migration_receipts").await,
        "rejected transition storage must not be deleted or rewritten"
    );
    assert_eq!(temporal_schema_version(&db_path).await, before_version);
    assert_eq!(
        temporal_schema_object_catalog(&db_path).await,
        before_catalog,
        "typed refusal must preserve every rejected temporal schema object"
    );
}

#[tokio::test]
async fn temporal_schema_rejects_retired_summary_sources_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal schema");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE TABLE session_summary_sources (retired_row INTEGER NOT NULL);
         INSERT INTO session_summary_sources(retired_row) VALUES (91);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let before_catalog = temporal_schema_object_catalog(&db_path).await;
    let before_version = temporal_schema_version(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("retired summary-source storage must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("retired summary-source storage must return typed reset-required");
    // `session_summary_sources` is the retired pre-Grafeo relational
    // authority, so the session-relation authority claims it ahead of the
    // temporal namespace scan — the same order production admission has
    // always used on reopen.
    assert_eq!(authority, "registered session relation store");
    assert!(
        reason.contains("session_summary_sources"),
        "unexpected reason: {reason}"
    );
    assert!(
        table_exists(&db_path, "session_summary_sources").await,
        "typed refusal must not delete retired summary-source storage"
    );
    assert_eq!(
        row_count(&db_path, "session_summary_sources").await,
        1,
        "typed refusal must not rewrite retired summary-source rows"
    );
    assert_eq!(temporal_schema_version(&db_path).await, before_version);
    assert_eq!(
        temporal_schema_object_catalog(&db_path).await,
        before_catalog,
        "typed refusal must preserve the rejected temporal schema"
    );
}

#[tokio::test]
async fn final_store_missing_temporal_marker_requires_reset_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh store should receive every final authority");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('admission-sentinel', 1, 'building', '{}', 100);
         DROP TABLE session_temporal_schema_migrations;",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let before_catalog = temporal_schema_object_catalog(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a final LCM store without temporal identity must require reset"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );
    assert!(
        !table_exists(&db_path, "session_temporal_schema_migrations").await,
        "typed refusal must not recreate a missing marker"
    );
    assert_eq!(
        row_count(&db_path, "session_temporal_generations").await,
        1,
        "typed refusal must not consume or repair retained temporal rows"
    );
    assert_eq!(
        temporal_schema_object_catalog(&db_path).await,
        before_catalog,
        "typed refusal must preserve the rejected temporal schema"
    );
}

#[tokio::test]
async fn temporal_schema_is_not_installed_into_a_nonempty_store() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch("CREATE TABLE authority_audit_checkpoints (wrong_column TEXT);")
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a nonempty store without temporal identity must require reset"),
        Err(error) => error,
    };
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _)| authority),
        Some("session temporal")
    );
    assert!(
        !table_exists(&db_path, "session_temporal_schema_migrations").await,
        "a rejected nonempty store must not gain a temporal marker: {error}"
    );
    assert!(
        !table_exists(&db_path, "session_summary_nodes").await,
        "a rejected nonempty store must not gain temporal authority tables"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_extra_temporal_column_metadata() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal schema");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "ALTER TABLE session_summary_nodes
         ADD COLUMN branch_local_metadata BLOB;",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("extra persisted temporal column metadata must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("extra temporal column metadata must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("session_summary_nodes"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        persisted_column_names(&db_path, "session_summary_nodes")
            .await
            .last()
            .map(String::as_str),
        Some("branch_local_metadata"),
        "typed refusal must not rewrite incompatible temporal column metadata"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_missing_temporal_occurrence_time_index() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal indexes");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch("DROP INDEX idx_session_occurrences_session_time;")
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);
    let rejected_catalog = temporal_schema_object_catalog(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a missing required temporal query index must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("a missing temporal index must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("session_occurrences") && reason.contains("session_id, knowledge_at"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        temporal_schema_object_catalog(&db_path).await,
        rejected_catalog,
        "typed refusal must not recreate the missing temporal index"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_missing_graph_publication_table() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install graph publication authority");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch("DROP TABLE graph_verified_heads_v1;")
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a missing graph publication table must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("a missing graph publication table must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("graph_verified_heads_v1"),
        "unexpected reason: {reason}"
    );
    assert!(
        !table_exists(&db_path, "graph_verified_heads_v1").await,
        "typed refusal must not recreate a missing graph publication table"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_incompatible_graph_publication_column_metadata() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install graph publication authority");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "ALTER TABLE graph_verified_heads_v1
         ADD COLUMN branch_local_metadata TEXT;",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("incompatible graph publication column metadata must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("incompatible graph metadata must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("graph_verified_heads_v1"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        persisted_column_names(&db_path, "graph_verified_heads_v1")
            .await
            .last()
            .map(String::as_str),
        Some("branch_local_metadata"),
        "typed refusal must not rewrite incompatible graph publication metadata"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_extra_graph_publication_table() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install graph publication authority");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE TABLE graph_publication_branch_local_v1 (
             retained_row INTEGER NOT NULL
         );
         INSERT INTO graph_publication_branch_local_v1(retained_row) VALUES (91);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("an extra graph publication table must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("an extra graph table must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("graph_publication_branch_local_v1"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        row_count(&db_path, "graph_publication_branch_local_v1").await,
        1,
        "typed refusal must not delete an extra graph publication object"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_mixed_case_extra_temporal_table() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install the final temporal schema");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE TABLE SeSsIoN_SuMmArY_BrAnCh_Local (
             retained_row INTEGER NOT NULL
         );
         INSERT INTO SeSsIoN_SuMmArY_BrAnCh_Local(retained_row) VALUES (91);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a mixed-case extra temporal table must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("a mixed-case temporal table must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("SeSsIoN_SuMmArY_BrAnCh_Local"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        row_count(&db_path, "SeSsIoN_SuMmArY_BrAnCh_Local").await,
        1,
        "typed refusal must not delete a mixed-case temporal object"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_mixed_case_extra_graph_publication_table() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install graph publication authority");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE TABLE GrApH_PuBlIcAtIoN_BrAnCh_Local (
             retained_row INTEGER NOT NULL
         );
         INSERT INTO GrApH_PuBlIcAtIoN_BrAnCh_Local(retained_row) VALUES (91);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("a mixed-case extra graph publication table must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("a mixed-case graph table must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("GrApH_PuBlIcAtIoN_BrAnCh_Local"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        row_count(&db_path, "GrApH_PuBlIcAtIoN_BrAnCh_Local").await,
        1,
        "typed refusal must not delete a mixed-case graph publication object"
    );
}

#[tokio::test]
async fn final_schema_admission_rejects_extra_index_on_canonical_graph_table() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("fresh initialization should install graph publication authority");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "CREATE INDEX branch_local_graph_head_digest
         ON graph_verified_heads_v1(recovered_digest);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("an extra index on a canonical graph table must require reset"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("an extra graph index must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains("branch_local_graph_head_digest"),
        "unexpected reason: {reason}"
    );
    assert!(
        schema_object_exists(&db_path, "index", "branch_local_graph_head_digest").await,
        "typed refusal must not delete an extra index on graph publication authority"
    );
}

#[tokio::test]
async fn temporal_schema_refuses_future_version_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);
    assert!(
        table_exists(&db_path, "session_temporal_schema_migrations").await,
        "the temporal schema must install a version marker before a future version is tested"
    );

    let before_catalog = temporal_schema_object_catalog(&db_path).await;
    let future_version = temporal_schema_version(&db_path).await + 97;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "UPDATE session_temporal_schema_migrations
         SET version = ?1
         WHERE name = 'session-temporal'",
        params![future_version],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("future.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let error = match open_global_db(&restart_path).await {
        Ok(_) => panic!("a newer temporal schema must be refused instead of treated as current"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .expect("a newer temporal marker must return typed reset-required");
    assert_eq!(authority, "session temporal");
    assert!(
        reason.contains(&format!("version {future_version}")),
        "unexpected reason: {reason}"
    );
    assert_eq!(temporal_schema_version(&restart_path).await, future_version);
    assert_eq!(
        temporal_schema_object_catalog(&restart_path).await,
        before_catalog
    );
}
