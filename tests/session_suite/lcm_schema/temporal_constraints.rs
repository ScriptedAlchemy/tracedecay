use super::*;

#[tokio::test]
async fn temporal_schema_rejects_cross_session_and_generation_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);
    assert!(
        table_exists(&db_path, "session_temporal_generations").await,
        "the temporal generation owner table must exist before ownership checks"
    );

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .await
        .unwrap();
    conn.execute_batch(
        "INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         )
         VALUES ('receipt-one', 'test', 'digest-one', '{}');
         INSERT INTO observations (
            observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json
         )
         VALUES ('observation-one', 'digest-one', 'receipt-one', '{}', '{}');
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         )
         VALUES ('anchor-one', '{}', '{}', 'test');
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         )
         VALUES
            ('session-one', 1, 'building', '{}', 100),
            ('session-one', 2, 'building', '{}', 100),
            ('session-two', 1, 'building', '{}', 100);
         INSERT INTO session_turns (
            session_id, generation, turn_id, ordinal, grouping_provenance, created_at
         )
         VALUES ('session-one', 1, 'turn-one', 0, 'provider', 100);
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
            valid_time_json, evidence_json, snippet_text, index_text
         )
         VALUES
            ('session-one', 1, 'occurrence-one', 'observation-one',
             0, 'anchor-one', 'assistant', 100,
             json_object('kind', 'unknown'), '{}', 'one', 'one'),
            ('session-one', 2, 'occurrence-two', 'observation-one',
             0, 'anchor-one', 'assistant', 100,
             json_object('kind', 'unknown'), '{}', 'two', 'two'),
            ('session-two', 1, 'occurrence-three', 'observation-one',
             0, 'anchor-one', 'assistant', 100,
             json_object('kind', 'unknown'), '{}', 'three', 'three');",
    )
    .await
    .unwrap();

    let cross_session = conn
        .execute(
            "INSERT INTO session_turn_members (
                session_id, generation, turn_id, occurrence_id, ordinal
             )
             VALUES ('session-one', 1, 'turn-one', 'occurrence-three', 0)",
            (),
        )
        .await;
    assert!(
        cross_session.is_err(),
        "a Turn cannot own an occurrence from another session"
    );

    let cross_generation = conn
        .execute(
            "INSERT INTO session_turn_members (
                session_id, generation, turn_id, occurrence_id, ordinal
             )
             VALUES ('session-one', 1, 'turn-one', 'occurrence-two', 0)",
            (),
        )
        .await;
    assert!(
        cross_generation.is_err(),
        "a Turn cannot own an occurrence from another generation"
    );

    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'session-one' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'active', activated_at = 102
         WHERE session_id = 'session-one' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'session-one' AND generation = 2",
        (),
    )
    .await
    .unwrap();
    let second_active = conn
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'active'
             WHERE session_id = 'session-one' AND generation = 2",
            (),
        )
        .await;
    assert!(
        second_active.is_err(),
        "only one active temporal generation is allowed per session"
    );
}

#[tokio::test]
async fn temporal_schema_rejects_invalid_current_assertion_and_valid_time_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         )
         VALUES ('receipt-one', 'test', 'digest-one', '{}');
         INSERT INTO observations (
            observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json
         )
         VALUES ('observation-one', 'digest-one', 'receipt-one', '{}', '{}');
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         )
         VALUES
            ('anchor-subject', '{}', '{}', 'test'),
            ('anchor-object', '{}', '{}', 'test');
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         )
         VALUES ('session-one', 1, 'building', '{}', 100);
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
            valid_time_json, evidence_json, snippet_text, index_text
         )
         VALUES (
            'session-one', 1, 'occurrence-one', 'observation-one',
            0, 'anchor-subject', 'assistant', 100,
            json_object('kind', 'known', 'valid_at', 100), '{}', 'one', 'one'
         );
         INSERT INTO session_assertions (
            session_id, generation, assertion_id, assertion_kind,
            subject_anchor_id, object_anchor_id, knowledge_at,
            valid_time_json, evidence_json
         )
         VALUES (
            'session-one', 1, 'assertion-one', 'corrects',
            'anchor-subject', 'anchor-object', 100,
            json_object('kind', 'known', 'valid_at', 100), '{}'
         );
         INSERT INTO session_current_entities (
            session_id, generation, entity_kind, entity_id,
            current_occurrence_id, coverage_json
         )
         VALUES (
            'session-one', 1, 'occurrence_anchor', 'anchor-subject',
            'occurrence-one', '{}'
         );",
    )
    .await
    .unwrap();

    for (sql, description) in [
        (
            "INSERT INTO session_current_entities (
                 session_id, generation, entity_kind, entity_id,
                 current_assertion_id, coverage_json
             )
             VALUES (
                 'session-one', 1, 'unsupported', 'anchor-subject',
                 'assertion-one', '{}'
             )",
            "current entities must use a typed entity kind",
        ),
        (
            "INSERT INTO session_current_entities (
                 session_id, generation, entity_kind, entity_id,
                 current_assertion_id, current_occurrence_id, coverage_json
             )
             VALUES (
                 'session-one', 1, 'occurrence_anchor', 'anchor-both',
                 'assertion-one', 'occurrence-one', '{}'
             )",
            "current entities must point to exactly one typed target",
        ),
        (
            "INSERT INTO session_assertions (
                 session_id, generation, assertion_id, assertion_kind,
                 subject_anchor_id, object_anchor_id, knowledge_at,
                 valid_time_json, evidence_json
             )
             VALUES (
                 'session-one', 1, 'assertion-invalid-kind', 'unsupported',
                 'anchor-subject', 'anchor-object', 100,
                 json_object('kind', 'known', 'valid_at', 100), '{}'
             )",
            "assertions must use a typed assertion kind",
        ),
        (
            "INSERT INTO session_assertions (
                 session_id, generation, assertion_id, assertion_kind,
                 subject_anchor_id, object_anchor_id, knowledge_at,
                 valid_time_json, evidence_json
             )
             VALUES (
                 'session-one', 1, 'assertion-invalid-time', 'corrects',
                 'anchor-subject', 'anchor-object', 100,
                 json_object('kind', 'known'), '{}'
             )",
            "known assertion valid time must include an integer valid_at",
        ),
        (
            "INSERT INTO session_occurrences (
                 session_id, generation, occurrence_id, source_observation_id,
                 projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                 valid_time_json, evidence_json, snippet_text, index_text
             )
             VALUES (
                 'session-one', 1, 'occurrence-invalid-time', 'observation-one',
                 1, 'anchor-subject', 'assistant', 101,
                 json_object('kind', 'unknown', 'valid_at', 101), '{}', 'bad', 'bad'
             )",
            "unknown occurrence valid time must not include valid_at",
        ),
    ] {
        assert!(
            conn.execute(sql, ()).await.is_err(),
            "schema accepted an invalid row: {description}"
        );
    }
}

#[tokio::test]
async fn temporal_schema_enforces_refresh_progress_and_terminal_receipts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-session', 1, 'building', '{}', 100);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-session', 'refresh-one',
            'sha256:0000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":10,\"committed_through\":4}',
            'running', 100, 100
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-session', 'refresh-one', 'session_store', 4, 10,
            'session-temporal-projector.v1',
            'sha256:0000000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:0000000000000000000000000000000000000000000000000000000000000000',
            100
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_operations (
                session_id, operation_id, request_digest, target_frontier_json,
                state, created_at, updated_at, terminal_at
             ) VALUES (
                'refresh-session', 'bad-start', 'digest',
                '{\"observed_through\":10,\"committed_through\":10}',
                'complete', 100, 101, 101
             )",
            (),
        )
        .await
        .is_err(),
        "refresh operations must start in running"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 0,
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 1, 99
             )",
            (),
        )
        .await
        .is_err(),
        "first progress cannot predate its owning operation"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 0,
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 1, 101
             )",
            (),
        )
        .await
        .is_err(),
        "progress without the operation generation's projection receipt must be rejected"
    );
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 1, 0,
            'sha256:1000000000000000000000000000000000000000000000000000000000000000',
            '{}', 4, 4,
            0, 'sha256:1100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1200000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1300000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1700000000000000000000000000000000000000000000000000000000000000',
            101
         )",
        (),
    )
    .await
    .unwrap();
    for (label, frontier, coverage) in [
        (
            "source minus one",
            "{\"observed_through\":10,\"committed_through\":3}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
        ),
        (
            "target plus one",
            "{\"observed_through\":10,\"committed_through\":11}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
        ),
        (
            "missing coverage fields",
            "{\"observed_through\":10,\"committed_through\":4}",
            "{}",
        ),
    ] {
        let sql = format!(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 0, '{frontier}', '{coverage}', 1, 0, 101
             )"
        );
        assert!(
            conn.execute(&sql, ()).await.is_err(),
            "{label} must be rejected"
        );
    }
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-session', 'refresh-one', 0,
            '{\"observed_through\":10,\"committed_through\":4}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 0, 101
         )",
        (),
    )
    .await
    .expect("first progress may commit at the binding source frontier (noop endpoint)");
    conn.execute(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('refresh-session', 'refresh-one', 0, 1, 0)",
        (),
    )
    .await
    .unwrap();
    conn.execute_batch(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 1, 1,
            'sha256:2000000000000000000000000000000000000000000000000000000000000000',
            '{}', 4, 10,
            0, 'sha256:2100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2200000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2300000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2700000000000000000000000000000000000000000000000000000000000000',
            102
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 1, 2,
            'sha256:2900000000000000000000000000000000000000000000000000000000000000',
            '{}', 10, 10,
            0, 'sha256:2910000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2920000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2930000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2940000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2950000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2960000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2970000000000000000000000000000000000000000000000000000000000000',
            103
         );",
    )
    .await
    .unwrap();

    for (label, values) in [
        (
            "batch regression",
            "1, '{\"observed_through\":10,\"committed_through\":5}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 1, 0, 102",
        ),
        (
            "record forge",
            "1, '{\"observed_through\":10,\"committed_through\":5}',
             '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 2, 1, 102",
        ),
        (
            "frontier regression",
            "1, '{\"observed_through\":10,\"committed_through\":3}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 2, 0, 102",
        ),
        (
            "timestamp regression",
            "1, '{\"observed_through\":10,\"committed_through\":5}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 2, 0, 100",
        ),
        (
            "ordinal gap",
            "2, '{\"observed_through\":10,\"committed_through\":10}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 3, 0, 103",
        ),
    ] {
        let sql = format!(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES ('refresh-session', 'refresh-one', {values})"
        );
        assert!(
            conn.execute(&sql, ()).await.is_err(),
            "{label} must be rejected"
        );
    }
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-session', 'refresh-one', 1,
            '{\"observed_through\":10,\"committed_through\":10}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            2, 0, 102
         )",
        (),
    )
    .await
    .expect(
        "subsequent progress may keep receipt.source_through at the prior committed endpoint while projection_through advances",
    );
    conn.execute_batch(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('refresh-session', 'refresh-one', 1, 1, 1);
         UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 103
         WHERE session_id = 'refresh-session' AND generation = 1;
         UPDATE session_temporal_generations
         SET state = 'active', activated_at = 104
         WHERE session_id = 'refresh-session' AND generation = 1;
         UPDATE session_refresh_operations
         SET state = 'complete', updated_at = 104, terminal_at = 104
         WHERE session_id = 'refresh-session' AND operation_id = 'refresh-one';",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 2,
                '{\"observed_through\":10,\"committed_through\":10}',
                '{\"visible\":2,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                3, 2, 105
             )",
            (),
        )
        .await
        .is_err(),
        "terminal operations cannot append progress"
    );

    for (label, terminal_state, frontier, coverage, terminal_at, failure_code) in [
        (
            "state mismatch",
            "failed",
            "{\"observed_through\":10,\"committed_through\":10}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            104,
            Some("boom"),
        ),
        (
            "frontier mismatch",
            "complete",
            "{\"observed_through\":10,\"committed_through\":9}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            104,
            None,
        ),
        (
            "coverage mismatch",
            "complete",
            "{\"observed_through\":10,\"committed_through\":10}",
            "{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            104,
            None,
        ),
        (
            "timestamp mismatch",
            "complete",
            "{\"observed_through\":10,\"committed_through\":10}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            105,
            None,
        ),
    ] {
        let failure_code =
            failure_code.map_or_else(|| "NULL".to_string(), |code| format!("'{code}'"));
        let sql = format!(
            "INSERT INTO session_refresh_receipts (
                session_id, operation_id, terminal_state, frontier_json,
                coverage_json, failure_code, terminal_at
             ) VALUES (
                'refresh-session', 'refresh-one', '{terminal_state}',
                '{frontier}', '{coverage}', {failure_code}, {terminal_at}
             )"
        );
        assert!(
            conn.execute(&sql, ()).await.is_err(),
            "{label} must be rejected"
        );
    }
    conn.execute(
        "INSERT INTO session_refresh_receipts (
            session_id, operation_id, terminal_state, frontier_json,
            coverage_json, failure_code, terminal_at
         ) VALUES (
            'refresh-session', 'refresh-one', 'complete',
            '{\"observed_through\":10,\"committed_through\":10}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            NULL, 104
         )",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_receipts SET terminal_at = 104
             WHERE session_id = 'refresh-session' AND operation_id = 'refresh-one'",
            (),
        )
        .await
        .is_err()
    );
    assert!(
        conn.execute(
            "DELETE FROM session_refresh_receipts
             WHERE session_id = 'refresh-session' AND operation_id = 'refresh-one'",
            (),
        )
        .await
        .is_err()
    );

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-session', 2, 'building', '{}', 200);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-session', 'refresh-failed',
            'sha256:3000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":10,\"committed_through\":4}',
            'running', 200, 200
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-session', 'refresh-failed', 'session_store', 4, 10,
            'session-temporal-projector.v1',
            'sha256:3000000000000000000000000000000000000000000000000000000000000000',
            2, '{}',
            'sha256:3000000000000000000000000000000000000000000000000000000000000000',
            200
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-failed', 0,
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 1, 200
             )",
            (),
        )
        .await
        .is_err(),
        "a progress row cannot borrow another operation's generation receipt"
    );
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 2, 0,
            'sha256:4000000000000000000000000000000000000000000000000000000000000000',
            '{}', 4, 4,
            0, 'sha256:4100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4200000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4300000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4700000000000000000000000000000000000000000000000000000000000000',
            200
         )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-session', 'refresh-failed', 0,
            '{\"observed_through\":10,\"committed_through\":4}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 0, 200
         )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('refresh-session', 'refresh-failed', 0, 2, 0)",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_operations
             SET state = 'cancelled', updated_at = 201, terminal_at = 201,
                 failure_code = 'must-not-survive'
             WHERE session_id = 'refresh-session' AND operation_id = 'refresh-failed'",
            (),
        )
        .await
        .is_err(),
        "cancelled operations cannot carry a failure code"
    );
    conn.execute_batch(
        "UPDATE session_temporal_generations
         SET state = 'failed', completed_at = 201
         WHERE session_id = 'refresh-session' AND generation = 2;
         UPDATE session_refresh_operations
         SET state = 'failed', updated_at = 201, terminal_at = 201, failure_code = 'boom'
         WHERE session_id = 'refresh-session' AND operation_id = 'refresh-failed';",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_receipts (
                session_id, operation_id, terminal_state, frontier_json,
                coverage_json, failure_code, terminal_at
             ) VALUES (
                'refresh-session', 'refresh-failed', 'failed',
                '{\"observed_through\":11,\"committed_through\":4}',
                '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                'boom', 201
             )",
            (),
        )
        .await
        .is_err(),
        "terminal receipt frontiers cannot exceed the owning target frontier"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_receipts (
                session_id, operation_id, terminal_state, frontier_json,
                coverage_json, failure_code, terminal_at
             ) VALUES (
                'refresh-session', 'refresh-failed', 'failed',
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                'other', 201
             )",
            (),
        )
        .await
        .is_err(),
        "terminal receipt failure codes must match the owning operation"
    );
    conn.execute(
        "INSERT INTO session_refresh_receipts (
            session_id, operation_id, terminal_state, frontier_json,
            coverage_json, failure_code, terminal_at
         ) VALUES (
            'refresh-session', 'refresh-failed', 'failed',
            '{\"observed_through\":10,\"committed_through\":4}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            'boom', 201
         )",
        (),
    )
    .await
    .unwrap();

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-zero', 1, 'building', '{}', 240);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-zero', 'zero-noop',
            'sha256:3200000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":0,\"committed_through\":0}',
            'running', 240, 240
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-zero', 'zero-noop', 'session_store', 0, 0,
            'session-temporal-projector.v1',
            'sha256:3200000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:3200000000000000000000000000000000000000000000000000000000000000',
            240
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-zero', 1, 0,
            'sha256:3300000000000000000000000000000000000000000000000000000000000000',
            '{}', 0, 0,
            0, 'sha256:3310000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3320000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3330000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3340000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3350000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3360000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3370000000000000000000000000000000000000000000000000000000000000',
            240
         );",
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-zero', 'zero-noop', 0,
            '{\"observed_through\":0,\"committed_through\":0}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 0, 240
         )",
        (),
    )
    .await
    .expect("zero-frontier empty first progress is a legal noop endpoint");

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-over-source', 1, 'building', '{}', 245);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-over-source', 'over-source',
            'sha256:3400000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":2,\"committed_through\":0}',
            'running', 245, 245
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-over-source', 'over-source', 'session_store', 0, 2,
            'session-temporal-projector.v1',
            'sha256:3400000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:3400000000000000000000000000000000000000000000000000000000000000',
            245
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-over-source', 1, 0,
            'sha256:3500000000000000000000000000000000000000000000000000000000000000',
            '{}', 1, 0,
            0, 'sha256:3510000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3520000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3530000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3540000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3550000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3560000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3570000000000000000000000000000000000000000000000000000000000000',
            245
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-over-source', 'over-source', 0,
                '{\"observed_through\":2,\"committed_through\":0}',
                '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 0, 245
             )",
            (),
        )
        .await
        .is_err(),
        "first progress must reject receipt.source_through past the committed endpoint"
    );

    conn.execute(
        "INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-orphan', 'orphan-terminal', 'orphan-digest',
            '{\"observed_through\":1,\"committed_through\":0}',
            'running', 250, 250
         )",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_operations
             SET state = 'failed', updated_at = 251, terminal_at = 251,
                 failure_code = 'forged'
             WHERE session_id = 'refresh-orphan' AND operation_id = 'orphan-terminal'",
            (),
        )
        .await
        .is_err(),
        "terminal operations must own a generation binding"
    );

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-forged-zero', 1, 'building', '{}', 300);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-forged-zero', 'forged-complete',
            'sha256:5000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":5,\"committed_through\":5}',
            'running', 300, 300
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-forged-zero', 'forged-complete', 'session_store', 5, 5,
            'session-temporal-projector.v1',
            'sha256:5000000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:5000000000000000000000000000000000000000000000000000000000000000',
            300
         );
         INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-forged-zero', 'forged-complete', 0,
            '{\"observed_through\":5,\"committed_through\":5}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            0, 0, 300
         );",
    )
    .await
    .unwrap();
    conn.execute_batch(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 301
         WHERE session_id = 'refresh-forged-zero' AND generation = 1;
         UPDATE session_temporal_generations
         SET state = 'active', activated_at = 302
         WHERE session_id = 'refresh-forged-zero' AND generation = 1;",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_operations
             SET state = 'complete', updated_at = 302, terminal_at = 302
             WHERE session_id = 'refresh-forged-zero' AND operation_id = 'forged-complete'",
            (),
        )
        .await
        .is_err(),
        "completion cannot be forged from the failure/cancellation zero-progress seed",
    );
}

#[tokio::test]
async fn temporal_schema_enforces_generation_state_machine_and_durability() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    assert!(
        conn.execute(
            "INSERT INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json, created_at, ready_at
             ) VALUES ('generation-session', 1, 'ready', '{}', 100, 101)",
            (),
        )
        .await
        .is_err(),
        "generation rows must start in building"
    );
    conn.execute(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES
            ('generation-session', 1, 'building', '{}', 100),
            ('generation-session', 2, 'building', '{}', 100)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'generation-session' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_temporal_generations
             SET ready_at = 102
             WHERE session_id = 'generation-session' AND generation = 1",
            (),
        )
        .await
        .is_err(),
        "same-state timestamp rewrites must be rejected"
    );
    assert!(
        conn.execute(
            "UPDATE session_temporal_generations
             SET state = 'superseded', activated_at = 102, completed_at = 103
             WHERE session_id = 'generation-session' AND generation = 1",
            (),
        )
        .await
        .is_err(),
        "ready cannot skip active"
    );
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'active', activated_at = 102
         WHERE session_id = 'generation-session' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'generation-session' AND generation = 2",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = 102
             WHERE session_id = 'generation-session' AND generation = 2",
            (),
        )
        .await
        .is_err(),
        "only one active generation is allowed"
    );
    assert!(
        conn.execute(
            "DELETE FROM session_temporal_generations
             WHERE session_id = 'generation-session' AND generation = 2",
            (),
        )
        .await
        .is_err(),
        "all generation rows are durable, including building generations"
    );
}

#[tokio::test]
async fn temporal_schema_keeps_append_only_authority_immutable() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = open_global_db(&db_path)
        .await
        .expect("temporal schema initialization should not error");
    drop(db);

    let raw_db = TestConnection::open(&db_path);
    let conn = (*raw_db).clone();
    conn.execute_batch(
        "INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES ('append-anchor', '{}', '{}', 'test');
         INSERT INTO session_summary_nodes (
            summary_id, session_id, summary_anchor_id, summary_text, index_text,
            source_horizon_json, created_at
         ) VALUES (
            'append-summary', 'append-session', 'append-anchor',
            'summary', 'summary', '{}', 100
         );
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('append-session', 1, 'building', '{}', 100);
         INSERT INTO session_temporal_migration_receipts (
            session_id, generation, batch_ordinal, source_digest,
            frozen_watermarks_json, imported_items, committed_at
         ) VALUES ('append-session', 1, 0, 'source', '{}', 1, 100);
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest,
            frozen_watermarks_json, source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'append-session', 1, 0, 'batch', '{}', 0, 0,
            0, 'occurrence', 0, 'dimension', 0, 'copy', 0, 'assertion',
            0, 'supersession', 0, 'current', 0, 'fts', 100
         );",
    )
    .await
    .unwrap();
    for sql in [
        "UPDATE session_summary_nodes SET summary_text = 'rewrite'
         WHERE summary_id = 'append-summary'",
        "DELETE FROM session_summary_nodes WHERE summary_id = 'append-summary'",
        "UPDATE session_temporal_migration_receipts SET imported_items = 2
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
        "DELETE FROM session_temporal_migration_receipts
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
        "UPDATE session_temporal_projection_receipts SET fts_digest = 'rewrite'
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
        "DELETE FROM session_temporal_projection_receipts
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
    ] {
        assert!(
            conn.execute(sql, ()).await.is_err(),
            "append-only authority mutation must be rejected: {sql}"
        );
    }
}
