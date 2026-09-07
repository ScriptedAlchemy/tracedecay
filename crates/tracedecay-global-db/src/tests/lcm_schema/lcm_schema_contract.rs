use super::*;
use tracedecay_domain::errors::TraceDecayError;

#[tokio::test]
async fn legacy_profile_requires_reset_without_carrying_session_content_forward() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let error = match open_global_db(&db_path).await {
        Err(error) => error,
        Ok(_) => panic!("legacy session content must require an explicit profile reset"),
    };
    assert!(matches!(
        error,
        TraceDecayError::ProfileResetRequired {
            component: "LCM",
            found_version: None,
            required_version: tracedecay_lcm::LCM_SCHEMA_VERSION,
        }
    ));
    assert!(!table_exists(&db_path, "lcm_raw_messages").await);
    assert_eq!(row_count(&db_path, "session_messages").await, 1);
}

#[tokio::test]
async fn stale_or_future_lcm_marker_requires_reset_without_rewriting_marker() {
    for found_version in [
        tracedecay_lcm::LCM_SCHEMA_VERSION - 1,
        tracedecay_lcm::LCM_SCHEMA_VERSION + 1,
    ] {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join(".tracedecay").join("sessions.db");
        let db = open_global_db(&db_path).await.expect("fresh global db");
        drop(db);
        set_migration_version(&db_path, found_version).await;

        let error = match open_global_db(&db_path).await {
            Err(error) => error,
            Ok(_) => panic!("incompatible LCM marker must require a reset"),
        };
        assert!(matches!(
            error,
            TraceDecayError::ProfileResetRequired {
                component: "LCM",
                found_version: Some(actual),
                required_version:
                    tracedecay_lcm::LCM_SCHEMA_VERSION,
            } if actual == found_version
        ));
        assert_eq!(schema_version(&db_path).await, found_version);
    }
}

#[tokio::test]
async fn current_lcm_schema_reopens_without_republishing_its_marker() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.expect("fresh global db");
    drop(db);
    set_migration_applied_at(&db_path, 123).await;

    let reopened = open_global_db(&db_path)
        .await
        .expect("current global db reopen");
    assert_eq!(
        schema_version_on(&reopened).await,
        tracedecay_lcm::LCM_SCHEMA_VERSION
    );
    drop(reopened);
    assert_eq!(migration_applied_at(&db_path).await, 123);
}

/// A store installed before the LCM status performance indexes existed is
/// already at the current LCM schema version, so the in-transaction LCM
/// stage skips it. A short-lived attach must still converge the indexes in
/// place — and retire the superseded plain payload owner index — without
/// touching the version marker or the rows those indexes cover.
#[tokio::test]
async fn short_lived_attach_convergence_rebuilds_queryable_lcm_status_indexes() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path).await.expect("fresh global db");
    drop(db);
    set_migration_applied_at(&db_path, 123).await;
    {
        // Rewind to the pre-index store shape while keeping the current
        // version marker, exactly as a store installed by an older binary.
        let db = TestConnection::open(&db_path);
        let conn = (*db).clone();
        conn.execute_batch(
            r#"INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', 'status-index-session', 'project.status-index', '/status-index');
             INSERT INTO lcm_raw_messages (
                 provider, message_id, session_id, role, ordinal, content,
                 content_hash, storage_kind, snippet_text, index_text,
                 legacy_truncated, metadata_json
             ) VALUES
                 (
                     'cursor', 'legacy-message', 'status-index-session', 'assistant', 1,
                     'legacy body', 'legacy-hash', 'inline', 'legacy', 'legacy', 1, NULL
                 ),
                 (
                     'cursor', 'lossy-message', 'status-index-session', 'assistant', 2,
                     'lossy body', 'lossy-hash', 'inline', 'lossy', 'lossy', 0,
                     '{"ingest_protection":{"lossy":true}}'
                 );
             INSERT INTO lcm_summary_nodes (
                 node_id, provider, conversation_id, session_id, depth,
                 summary_text, summary_hash, summary_token_count, source_token_count
             ) VALUES (
                 'summary-node', 'cursor', 'conversation', 'status-index-session', 2,
                 'summary body', 'summary-hash', 3, 5
             );
             INSERT INTO lcm_external_payloads (
                 payload_ref, provider, session_id, message_id, kind,
                 content_hash, byte_count, char_count
             ) VALUES (
                 'payload-ref', 'cursor', 'status-index-session', 'payload-message', 'text',
                 'payload-hash', 11, 7
             );
             DROP INDEX idx_lcm_raw_legacy_truncated;
             DROP INDEX idx_lcm_raw_lossy_ingest;
             DROP INDEX idx_lcm_summary_nodes_depth_tokens;
             DROP INDEX idx_lcm_external_payloads_owner_bytes;
             CREATE INDEX idx_lcm_external_payloads_owner
                 ON lcm_external_payloads(provider, session_id);"#,
        )
        .await
        .expect("rewind to the pre-index schema shape");
    }

    let reopened = open_global_db(&db_path)
        .await
        .expect("pre-index store reopen");
    let raw_indexes = table_index_names(&reopened, "lcm_raw_messages").await;
    for index in ["idx_lcm_raw_legacy_truncated", "idx_lcm_raw_lossy_ingest"] {
        assert!(
            raw_indexes.iter().any(|name| name == index),
            "short-lived convergence did not build {index}; raw message indexes: {raw_indexes:?}"
        );
    }
    let summary_indexes = table_index_names(&reopened, "lcm_summary_nodes").await;
    assert!(
        summary_indexes
            .iter()
            .any(|name| name == "idx_lcm_summary_nodes_depth_tokens"),
        "short-lived convergence did not build the summary depth/token index: {summary_indexes:?}"
    );
    let payload_indexes = table_index_names(&reopened, "lcm_external_payloads").await;
    assert!(
        payload_indexes
            .iter()
            .any(|name| name == "idx_lcm_external_payloads_owner_bytes"),
        "short-lived convergence did not build the payload owner/bytes index: {payload_indexes:?}"
    );
    assert!(
        !payload_indexes
            .iter()
            .any(|name| name == "idx_lcm_external_payloads_owner"),
        "short-lived convergence left the superseded payload owner index in place: {payload_indexes:?}"
    );
    for (index, query) in [
        (
            "idx_lcm_raw_legacy_truncated",
            "SELECT COUNT(*)
             FROM lcm_raw_messages INDEXED BY idx_lcm_raw_legacy_truncated
             WHERE provider = ?1 AND session_id = ?2 AND legacy_truncated != 0",
        ),
        (
            "idx_lcm_raw_lossy_ingest",
            "SELECT COUNT(*)
             FROM lcm_raw_messages INDEXED BY idx_lcm_raw_lossy_ingest
             WHERE provider = ?1 AND session_id = ?2
               AND metadata_json IS NOT NULL
               AND json_valid(metadata_json)
               AND json_type(metadata_json, '$.ingest_protection.lossy') = 'true'",
        ),
        (
            "idx_lcm_summary_nodes_depth_tokens",
            "SELECT COUNT(*)
             FROM lcm_summary_nodes INDEXED BY idx_lcm_summary_nodes_depth_tokens
             WHERE provider = ?1 AND session_id = ?2",
        ),
        (
            "idx_lcm_external_payloads_owner_bytes",
            "SELECT COUNT(*)
             FROM lcm_external_payloads INDEXED BY idx_lcm_external_payloads_owner_bytes
             WHERE provider = ?1 AND session_id = ?2",
        ),
    ] {
        let mut rows = reopened
            .query(query, params!["cursor", "status-index-session"])
            .await
            .unwrap_or_else(|error| panic!("rebuilt index {index} must remain queryable: {error}"));
        let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 1, "rebuilt index {index} lost its covered row");
    }
    assert_eq!(
        schema_version_on(&reopened).await,
        tracedecay_lcm::LCM_SCHEMA_VERSION
    );
    drop(reopened);
    assert_eq!(migration_applied_at(&db_path).await, 123);
}
