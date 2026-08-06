use super::*;

#[tokio::test]
async fn temporal_schema_complete_object_catalog() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");

    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let mut expected = TEMPORAL_SCHEMA_OBJECTS
        .iter()
        .map(|(object_type, object_name)| ((*object_type).to_string(), (*object_name).to_string()))
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(temporal_schema_object_catalog(&db_path).await, expected);
    assert!(
        table_exists(&db_path, "lcm_raw_messages").await,
        "the additive temporal schema must preserve legacy LCM tables"
    );
}

#[tokio::test]
async fn temporal_payload_manifest_schema_is_payload_global() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_info('session_external_payload_manifests')
             ORDER BY cid",
            (),
        )
        .await
        .unwrap();
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        columns.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(
        columns,
        [
            "payload_ref",
            "session_id",
            "payload_digest",
            "manifest_json",
            "receipt_id",
            "created_at",
        ]
    );

    let mut rows = conn
        .query(
            "SELECT \"from\", \"table\", \"to\"
             FROM pragma_foreign_key_list('session_external_payload_manifests')
             ORDER BY \"from\"",
            (),
        )
        .await
        .unwrap();
    let mut foreign_keys = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        foreign_keys.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<String>(2).unwrap(),
        ));
    }
    assert_eq!(
        foreign_keys,
        [(
            "receipt_id".to_string(),
            "sanitization_receipts".to_string(),
            "receipt_id".to_string(),
        )]
    );

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('cursor', 'manifest-owner', '/tmp/project', '/tmp/project');
         INSERT INTO sanitization_receipts (
             receipt_id, sanitizer_version, payload_digest, receipt_json
         ) VALUES ('manifest-receipt', 'test', 'digest', '{}');
         INSERT INTO lcm_external_payloads (
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, created_at
         ) VALUES
             ('payload-owned', 'cursor', 'manifest-owner', 'message-owned',
              'tool', 'digest', 1, 1, 100),
             ('payload-cross-session', 'cursor', 'manifest-owner', 'message-cross',
              'tool', 'digest', 1, 1, 100);
         INSERT INTO session_external_payload_manifests (
             payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
         ) VALUES (
             'payload-owned', 'manifest-owner', 'digest', '{}', 'manifest-receipt', 100
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_external_payload_manifests (
                 payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
             ) VALUES (
                 'payload-cross-session', 'different-owner', 'digest', '{}',
                 'manifest-receipt', 100
             )",
            (),
        )
        .await
        .is_err(),
        "a payload manifest owner must match raw payload authority"
    );
    for sql in [
        "UPDATE session_external_payload_manifests SET payload_digest = 'rewrite'
         WHERE payload_ref = 'payload-owned'",
        "DELETE FROM session_external_payload_manifests WHERE payload_ref = 'payload-owned'",
    ] {
        assert!(
            conn.execute(sql, ()).await.is_err(),
            "payload-global authority must remain immutable: {sql}"
        );
    }
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
        table_exists(&db_path, "session_temporal_schema_state").await,
        "the temporal schema must install a version marker before a future version is tested"
    );

    let before_catalog = temporal_schema_object_catalog(&db_path).await;
    let future_version = temporal_schema_version(&db_path).await + 97;
    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute(
        "UPDATE session_temporal_schema_state
         SET version = ?1
         WHERE domain = 'session-temporal'",
        params![future_version],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("future.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    assert!(
        open_global_db(&restart_path).await.is_err(),
        "a newer temporal schema must be refused instead of treated as current"
    );
    assert_eq!(temporal_schema_version(&restart_path).await, future_version);
    assert_eq!(
        temporal_schema_object_catalog(&restart_path).await,
        before_catalog
    );
}

#[tokio::test]
async fn temporal_schema_query_indexes_cover_exact_lookup_shapes() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    for (sql, index) in [
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND retrieval_anchor_id = 'anchor-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_anchor_order",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND message_id = 'message-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_message",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND thread_id = 'thread-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_thread",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND turn_id = 'turn-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_turn",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND agent_id = 'agent-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_agent",
        ),
        (
            "SELECT entity_id
             FROM session_current_entities
             WHERE session_id = 'session-one'
               AND generation = 1
               AND current_occurrence_id = 'occurrence-one'",
            "idx_session_current_entities_occurrence",
        ),
        (
            "SELECT assertion_id
             FROM session_assertions
             WHERE session_id = 'session-one'
               AND generation = 1
               AND object_anchor_id = 'anchor-object'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, assertion_id",
            "idx_session_assertions_object_order",
        ),
        (
            "SELECT assertion_id
             FROM session_assertions
             WHERE session_id = 'session-one'
               AND generation = 1
               AND assertion_kind = 'corrects'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, assertion_id",
            "idx_session_assertions_kind_order",
        ),
        (
            "SELECT assertion_id
             FROM session_assertions
             WHERE session_id = 'session-one'
               AND generation = 1
               AND knowledge_at >= 0
             ORDER BY knowledge_at, assertion_id",
            "idx_session_assertions_generation_order",
        ),
        (
            "SELECT payload_ref
             FROM session_external_payload_manifests
             WHERE session_id = 'session-one'",
            "idx_session_external_payload_manifests_session",
        ),
    ] {
        let details = explain_query_plan(&conn, sql).await;
        assert!(
            details.iter().any(|detail| detail.contains(index)),
            "EXPLAIN did not use {index} for `{sql}`: {details:?}"
        );
    }
}

#[tokio::test]
async fn temporal_schema_root_retrieval_indexes_cover_catalog_and_large_query_shapes() {
    const OCCURRENCE_ROOT_INDEX: &str = "idx_session_occurrences_root_generation_order";
    const SUMMARY_ROOT_INDEX: &str = "idx_session_summary_nodes_root_created_order";

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    for (table, index, expected_columns, root_prefix) in [
        (
            "session_occurrences",
            OCCURRENCE_ROOT_INDEX,
            &["knowledge_at", "session_id", "occurrence_id", "generation"][..],
            "idx_session_occurrences_root_",
        ),
        (
            "session_summary_nodes",
            SUMMARY_ROOT_INDEX,
            &["created_at", "session_id", "summary_id"][..],
            "idx_session_summary_nodes_root_",
        ),
    ] {
        let expected = expected_columns
            .iter()
            .map(|column| ((*column).to_owned(), 0))
            .collect::<Vec<_>>();
        assert_eq!(
            index_key_columns(&conn, index).await,
            expected,
            "{index} must retain its exact ascending key contract"
        );

        let index_names = table_index_names(&conn, table).await;
        let mut matching_keysets = Vec::new();
        for candidate in &index_names {
            if index_key_columns(&conn, candidate).await == expected {
                matching_keysets.push(candidate.clone());
            }
        }
        assert_eq!(
            matching_keysets,
            [index.to_owned()],
            "{table} must not retain a redundant index with the root keyset"
        );

        let root_indexes = index_names
            .into_iter()
            .filter(|candidate| candidate.starts_with(root_prefix))
            .collect::<Vec<_>>();
        assert_eq!(
            root_indexes,
            [index.to_owned()],
            "{table} must not retain a conflicting root index variant"
        );
    }

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         ) VALUES ('root-receipt', 'test', 'root-digest', '{}');
         INSERT INTO observations (
            observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json
         ) VALUES (
            'root-observation',
            'root-digest',
            'root-receipt',
            '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}',
            '{}'
         );
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES ('root-anchor', '{}', '{}', 'test');
         WITH RECURSIVE sequence(value) AS (
            VALUES(0)
            UNION ALL
            SELECT value + 1 FROM sequence WHERE value < 7
         )
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         )
         SELECT printf('root-session-%02d', value), 1, 'building', '{}', 0
         FROM sequence;
         UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 1;
         UPDATE session_temporal_generations
         SET state = 'active', activated_at = 2;
         WITH RECURSIVE sequence(value) AS (
            VALUES(0)
            UNION ALL
            SELECT value + 1 FROM sequence WHERE value < 99999
         )
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
            valid_time_json, evidence_json, snippet_text, index_text
         )
         SELECT
            printf('root-session-%02d', value % 8),
            1,
            printf('root-occurrence-%06d', value),
            'root-observation',
            value,
            'root-anchor',
            'assistant',
            value / 8,
            json_object('kind', 'unknown'),
            '{}',
            'root occurrence',
            'root occurrence'
         FROM sequence;
         WITH RECURSIVE sequence(value) AS (
            VALUES(0)
            UNION ALL
            SELECT value + 1 FROM sequence WHERE value < 99999
         )
         INSERT INTO session_summary_nodes (
            summary_id, session_id, summary_anchor_id, summary_text, index_text,
            source_horizon_json, created_at
         )
         SELECT
            printf('root-summary-%06d', value),
            printf('root-session-%02d', value % 8),
            'root-anchor',
            'root summary',
            'root summary',
            '{}',
            value / 8
         FROM sequence;
         ANALYZE;",
    )
    .await
    .unwrap();

    for table in ["session_occurrences", "session_summary_nodes"] {
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 100_000, "{table} fixture must stay planner-scale");
    }

    for (shape, sql, index) in [
        (
            "root occurrence candidate",
            "SELECT o.occurrence_id
             FROM session_temporal_generations AS frozen
             JOIN session_occurrences AS o
               INDEXED BY idx_session_occurrences_root_generation_order
               ON o.session_id = frozen.session_id
              AND o.generation = frozen.generation
             JOIN observations AS provider_observation
               ON provider_observation.observation_id = o.source_observation_id
             WHERE frozen.state = 'active'
               AND (NULL IS NULL OR json_extract(
                   provider_observation.observation_json, '$.identity.source.provider'
               ) = NULL)
               AND o.knowledge_at >= 0
               AND o.knowledge_at < 12500
               AND (
                   o.knowledge_at < 9223372036854775807
                   OR (
                       o.knowledge_at = 9223372036854775807
                       AND (
                           o.session_id > ''
                           OR (o.session_id = '' AND o.occurrence_id > '')
                       )
                   )
               )
             ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
             LIMIT 38",
            OCCURRENCE_ROOT_INDEX,
        ),
        (
            "root occurrence pagination",
            "SELECT o.occurrence_id
             FROM session_temporal_generations AS frozen
             JOIN session_occurrences AS o
               INDEXED BY idx_session_occurrences_root_generation_order
               ON o.session_id = frozen.session_id
              AND o.generation = frozen.generation
             JOIN observations AS provider_observation
               ON provider_observation.observation_id = o.source_observation_id
             WHERE frozen.state = 'active'
               AND (NULL IS NULL OR json_extract(
                   provider_observation.observation_json, '$.identity.source.provider'
               ) = NULL)
               AND o.knowledge_at >= 0
               AND o.knowledge_at < 12500
               AND (
                   o.knowledge_at < 7111
                   OR (
                       o.knowledge_at = 7111
                       AND (
                           o.session_id > 'root-session-03'
                           OR (
                               o.session_id = 'root-session-03'
                               AND o.occurrence_id > 'root-occurrence-057000'
                           )
                       )
                   )
               )
             ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
             LIMIT 38",
            OCCURRENCE_ROOT_INDEX,
        ),
        (
            "root occurrence provider filter",
            "SELECT o.occurrence_id
             FROM session_temporal_generations AS frozen
             JOIN session_occurrences AS o
               INDEXED BY idx_session_occurrences_root_generation_order
               ON o.session_id = frozen.session_id
              AND o.generation = frozen.generation
             JOIN observations AS provider_observation
               ON provider_observation.observation_id = o.source_observation_id
             WHERE frozen.state = 'active'
               AND ('claude' IS NULL OR json_extract(
                   provider_observation.observation_json, '$.identity.source.provider'
               ) = 'claude')
               AND o.knowledge_at >= 0
               AND o.knowledge_at < 12500
               AND (
                   o.knowledge_at < 7111
                   OR (
                       o.knowledge_at = 7111
                       AND (
                           o.session_id > 'root-session-03'
                           OR (
                               o.session_id = 'root-session-03'
                               AND o.occurrence_id > 'root-occurrence-057000'
                           )
                       )
                   )
               )
             ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
             LIMIT 38",
            OCCURRENCE_ROOT_INDEX,
        ),
        (
            "root summary candidate",
            "SELECT summary_id
             FROM session_summary_nodes
             WHERE created_at >= 0
               AND created_at < 12500
               AND (
                   created_at < 7111
                   OR (
                       created_at = 7111
                       AND (
                           session_id > 'root-session-03'
                           OR (
                               session_id = 'root-session-03'
                               AND summary_id > 'root-summary-057000'
                           )
                       )
                   )
               )
             ORDER BY created_at DESC, session_id, summary_id
             LIMIT 38",
            SUMMARY_ROOT_INDEX,
        ),
    ] {
        let details = explain_query_plan(&conn, sql).await;
        assert!(
            details.iter().any(|detail| detail.contains(index)),
            "EXPLAIN did not use {index} for {shape}: {details:?}"
        );
    }
}
