#![allow(clippy::assertions_on_constants)] // intentional const assertion
use super::*;

#[tokio::test]
async fn lcm_schema_migrates_legacy_sessions_db_in_place() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = open_global_db(&db_path).await.expect("global db open");
    assert_eq!(
        schema_version_on(&db).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );

    let legacy = tracedecay_sessions::runtime::lcm::schema::load_raw_message(
        &*db,
        "cursor",
        "legacy-message",
    )
    .await
    .expect("legacy message should be carried into raw store");
    assert_eq!(legacy.provider, "cursor");
    assert_eq!(legacy.message_id, "legacy-message");
    assert_eq!(legacy.session_id, "legacy-session");
    assert_eq!(legacy.role, "assistant");
    assert_eq!(legacy.ordinal, 1);
    assert_eq!(legacy.content, "legacy text");
    assert_eq!(
        legacy.storage_kind,
        tracedecay_sessions::runtime::lcm::LcmStorageKind::Inline
    );
    assert!(legacy.legacy_source);
    assert!(!legacy.legacy_truncated);
    drop(db);

    assert!(table_exists(&db_path, "session_schema_migrations").await);
    assert!(table_exists(&db_path, "lcm_raw_messages").await);
    assert!(table_exists(&db_path, "lcm_raw_messages_fts").await);
    assert_eq!(
        fts_legacy_message_ids(&db_path).await,
        vec!["legacy-message".to_string()]
    );
}

#[tokio::test]
async fn lcm_schema_marks_legacy_truncated_messages() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let legacy_text = "legacy text\n[truncated by tracedecay]";
    create_legacy_sessions_db_with_text(&db_path, legacy_text).await;

    let db = open_global_db(&db_path).await.expect("global db open");
    let legacy = tracedecay_sessions::runtime::lcm::schema::load_raw_message(
        &*db,
        "cursor",
        "legacy-message",
    )
    .await
    .expect("legacy message should be carried into raw store");

    assert_eq!(legacy.content, legacy_text);
    assert!(legacy.legacy_source);
    assert!(legacy.legacy_truncated);
}

#[tokio::test]
async fn lcm_schema_migration_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = open_global_db(&db_path).await.expect("global db open");
    assert_eq!(
        schema_version_on(&db).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    let reopened = open_global_db(&db_path).await.expect("global db reopen");
    assert_eq!(
        schema_version_on(&reopened).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(reopened);
    assert_eq!(
        schema_version(&db_path).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(row_count(&db_path, "lcm_raw_messages").await, 1);
    assert_eq!(
        fts_legacy_message_ids(&db_path).await,
        vec!["legacy-message".to_string()]
    );
}

#[tokio::test]
async fn lcm_schema_v6_migrates_bounded_codex_pending_queue_indexes() {
    const SESSION_QUERY: &str = "
        SELECT candidate.node_id, candidate.session_id
        FROM lcm_summary_nodes AS candidate
        JOIN session_summary_nodes AS authority
          ON authority.summary_id = candidate.node_id
         AND authority.session_id = candidate.session_id
        WHERE candidate.provider = 'codex'
          AND CASE
                WHEN json_valid(candidate.metadata_json) THEN
                  json_extract(candidate.metadata_json, '$.source') =
                    'codex_context_compacted'
                  AND COALESCE(
                        json_extract(
                          candidate.metadata_json,
                          '$.tracedecay_summary_source'
                        ),
                        ''
                      ) <> 'codex_app_server'
                ELSE 0
              END = 1
          AND NOT EXISTS (
                SELECT 1
                FROM session_summary_successors AS lineage
                WHERE lineage.predecessor_summary_id = candidate.node_id
              )
          AND EXISTS (
                SELECT 1
                FROM lcm_summary_sources AS source
                JOIN lcm_raw_messages AS raw
                  ON source.source_kind = 'raw_message'
                 AND CAST(source.source_id AS INTEGER) = raw.store_id
                 AND raw.provider = candidate.provider
                 AND raw.session_id = candidate.session_id
                WHERE source.node_id = candidate.node_id
              )
          AND candidate.session_id = 'session-one'
        ORDER BY candidate.depth DESC, candidate.created_at DESC, candidate.node_id
        LIMIT 10";
    const ROOT_QUERY: &str = "
        SELECT candidate.node_id, candidate.session_id
        FROM lcm_summary_nodes AS candidate
        JOIN session_summary_nodes AS authority
          ON authority.summary_id = candidate.node_id
         AND authority.session_id = candidate.session_id
        WHERE candidate.provider = 'codex'
          AND CASE
                WHEN json_valid(candidate.metadata_json) THEN
                  json_extract(candidate.metadata_json, '$.source') =
                    'codex_context_compacted'
                  AND COALESCE(
                        json_extract(
                          candidate.metadata_json,
                          '$.tracedecay_summary_source'
                        ),
                        ''
                      ) <> 'codex_app_server'
                ELSE 0
              END = 1
          AND NOT EXISTS (
                SELECT 1
                FROM session_summary_successors AS lineage
                WHERE lineage.predecessor_summary_id = candidate.node_id
              )
          AND EXISTS (
                SELECT 1
                FROM lcm_summary_sources AS source
                JOIN lcm_raw_messages AS raw
                  ON source.source_kind = 'raw_message'
                 AND CAST(source.source_id AS INTEGER) = raw.store_id
                 AND raw.provider = candidate.provider
                 AND raw.session_id = candidate.session_id
                WHERE source.node_id = candidate.node_id
              )
        ORDER BY candidate.created_at DESC, candidate.depth DESC, candidate.node_id
        LIMIT 10";

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.expect("global db open");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_lcm_summary_nodes_codex_pending_session_order;
         DROP INDEX IF EXISTS idx_lcm_summary_nodes_codex_pending_root_order;
         CREATE INDEX idx_lcm_summary_nodes_codex_pending_session_order
             ON lcm_summary_nodes(session_id, depth DESC, created_at DESC, node_id)
             WHERE provider = 'codex'
               AND CASE
                     WHEN json_valid(metadata_json) THEN
                       json_extract(metadata_json, '$.source') = 'codex_context_compacted'
                       AND COALESCE(
                             json_extract(metadata_json, '$.tracedecay_summary_source'),
                             ''
                           ) <> 'codex_app_server'
                     ELSE 0
                   END;
         CREATE INDEX idx_lcm_summary_nodes_codex_pending_root_order
             ON lcm_summary_nodes(created_at DESC, depth DESC, node_id, session_id)
             WHERE provider = 'codex'
               AND CASE
                     WHEN json_valid(metadata_json) THEN
                       json_extract(metadata_json, '$.source') = 'codex_context_compacted'
                       AND COALESCE(
                             json_extract(metadata_json, '$.tracedecay_summary_source'),
                             ''
                           ) <> 'codex_app_server'
                     ELSE 0
                   END;
         UPDATE session_schema_migrations SET version = 6 WHERE name = 'lcm';",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    #[allow(clippy::assertions_on_constants)]
    {
        assert!(tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION > 6);
    }
    let migrated = open_global_db(&db_path)
        .await
        .expect("v6 database should migrate");
    assert_eq!(
        schema_version_on(&migrated).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(migrated);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    assert_eq!(
        index_key_columns(&conn, "idx_lcm_summary_nodes_codex_pending_session_order").await,
        vec![
            ("session_id".to_string(), 0),
            ("<expression>".to_string(), 0),
            ("depth".to_string(), 1),
            ("created_at".to_string(), 1),
            ("node_id".to_string(), 0),
        ]
    );
    assert_eq!(
        index_key_columns(&conn, "idx_lcm_summary_nodes_codex_pending_root_order").await,
        vec![
            ("<expression>".to_string(), 0),
            ("created_at".to_string(), 1),
            ("depth".to_string(), 1),
            ("node_id".to_string(), 0),
            ("session_id".to_string(), 0),
        ]
    );

    for (query, expected_index) in [
        (
            SESSION_QUERY,
            "idx_lcm_summary_nodes_codex_pending_session_order",
        ),
        (ROOT_QUERY, "idx_lcm_summary_nodes_codex_pending_root_order"),
    ] {
        let details = explain_query_plan(&conn, query).await;
        assert!(
            details.iter().any(|detail| detail.contains(expected_index)),
            "EXPLAIN did not use {expected_index}: {details:?}"
        );
        assert!(
            details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
            "pending query must not sort through a temporary B-tree: {details:?}"
        );
        assert!(
            details.iter().any(|detail| {
                detail.contains("sqlite_autoindex_session_summary_successors_1")
                    && detail.contains("predecessor_summary_id=?")
            }),
            "leaf anti-join must use the successor primary key: {details:?}"
        );
    }
}

// Schema v3 narrows the raw-message FTS index to index_text only, matching
// hermes-lcm `build_message_fts_spec` (store.py:173-204) which indexes the
// content column alone. Migrating a v2 database must restructure the FTS
// objects, carry the searchable rows forward, and stop role/metadata text
// from satisfying unqualified MATCH queries.
#[tokio::test]
async fn lcm_schema_v3_migration_restructures_raw_fts_and_preserves_search() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    // Establish the schema, then rewrite the FTS objects into the pre-v3
    // shape with the version marker set back to 2.
    let db = open_global_db(&db_path).await.expect("global db open");
    drop(db);
    downgrade_raw_fts_to_v2(&db_path).await;
    assert_eq!(schema_version(&db_path).await, 2);
    assert_eq!(
        fts_message_ids_matching(&db_path, "assistant").await,
        vec!["legacy-message".to_string()],
        "v2 fixture must over-match via the indexed role column"
    );

    let migrated = open_global_db(&db_path).await.expect("global db reopen");
    assert_eq!(
        schema_version_on(&migrated).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(migrated);

    // The restructured objects no longer index role/metadata_json.
    let sqls = raw_fts_object_sql(&db_path).await;
    assert_eq!(sqls.len(), 4, "FTS table and three triggers must exist");
    for sql in &sqls {
        assert!(
            !sql.contains("metadata_json"),
            "migrated FTS object still references metadata_json: {sql}"
        );
    }

    // Search results carried forward; role text no longer matches.
    assert_eq!(
        fts_message_ids_matching(&db_path, "legacy").await,
        vec!["legacy-message".to_string()],
        "content search results must survive the migration"
    );
    assert!(
        fts_message_ids_matching(&db_path, "assistant")
            .await
            .is_empty(),
        "role text must not match after the v3 restructure"
    );

    // Idempotent re-open: structure and results are stable.
    let reopened = open_global_db(&db_path).await.expect("idempotent reopen");
    assert_eq!(
        schema_version_on(&reopened).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(reopened);
    assert_eq!(
        fts_message_ids_matching(&db_path, "legacy").await,
        vec!["legacy-message".to_string()]
    );
    assert!(
        fts_message_ids_matching(&db_path, "assistant")
            .await
            .is_empty()
    );
}

// Mirrors hermes-lcm `run_versioned_migrations` (db_bootstrap.py:580-601):
// version steps are monotonic and `set_schema_version(conn, current_version)`
// never lowers a marker written by a newer release. Opening a database whose
// LCM schema version is newer than this binary must not downgrade the marker
// or re-run the legacy carry-forward against data the newer schema owns.
#[tokio::test]
async fn lcm_schema_future_version_is_preserved_without_remigration() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = open_global_db(&db_path).await.expect("global db open");
    assert_eq!(
        schema_version_on(&db).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    // Simulate a database last touched by a newer tracedecay: bump the version
    // marker past this binary and have the newer schema relocate carried rows
    // out of lcm_raw_messages.
    let future_version = tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION + 97;
    set_migration_version(&db_path, future_version).await;
    set_migration_applied_at(&db_path, 456).await;
    {
        let raw_db = TestConnection::open(&db_path);
        let conn = (*raw_db).clone();
        conn.execute("DELETE FROM lcm_raw_messages", ())
            .await
            .unwrap();
    }
    assert_eq!(row_count(&db_path, "lcm_raw_messages").await, 0);

    let reopened = open_global_db(&db_path).await.expect("global db reopen");
    assert_eq!(
        schema_version_on(&reopened).await,
        future_version,
        "future schema version marker must not be downgraded"
    );
    drop(reopened);
    assert_eq!(schema_version(&db_path).await, future_version);
    assert_eq!(migration_applied_at(&db_path).await, 456);
    assert_eq!(
        row_count(&db_path, "lcm_raw_messages").await,
        0,
        "legacy carry-forward must not re-run against a newer schema's data"
    );
}

#[tokio::test]
async fn lcm_schema_current_version_reopen_skips_migration_update() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = open_global_db(&db_path).await.expect("global db open");
    assert_eq!(
        schema_version_on(&db).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    set_migration_applied_at(&db_path, 123).await;
    assert_eq!(migration_applied_at(&db_path).await, 123);

    let reopened = open_global_db(&db_path).await.expect("global db reopen");
    assert_eq!(
        schema_version_on(&reopened).await,
        tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
    );
    drop(reopened);
    assert_eq!(migration_applied_at(&db_path).await, 123);
}
