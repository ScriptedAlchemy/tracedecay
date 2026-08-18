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
         VALUES ('unexpected-temporal-marker', 3, 91);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let before_catalog = temporal_schema_object_catalog(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Ok(_) => panic!("an exact-v3 marker plus extra rows must require reset"),
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
            ("session-temporal".to_string(), 3, 90),
            ("unexpected-temporal-marker".to_string(), 3, 91),
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
