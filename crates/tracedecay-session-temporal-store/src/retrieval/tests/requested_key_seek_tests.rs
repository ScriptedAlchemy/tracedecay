//! Requested-key lookups inside one session must seek each key. Turso seeks an
//! `IN` only on a leading index column, so `session_id = ?1 AND key IN (...)`
//! reads every row of the session; each statement here joins its requested
//! keys instead. Every probe runs on the same session before and after it
//! grows eightfold with rows none of the keys match, and its VM steps must not
//! follow that growth.

use super::*;

const SESSION: &str = "session-snapshot";
const BASE_UNRELATED: usize = 2_000;
const GROWN_UNRELATED: usize = 16_000;
const EVIDENCE: &str = r#"{"authority":"provider_native","evidence_class":"provider_declared","source_anchor_id":"seek-evidence-anchor","sanitization_receipt":{"receipt_id":"seek-receipt","sanitizer_version":"seek-sanitizer"}}"#;

fn target_rows_sql() -> String {
    format!(
        "INSERT INTO sessions (provider, session_id, project_key, project_path)
         VALUES ('claude', '{SESSION}', 'user', '/requested-key-seek');
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('{SESSION}', 1, 'building', '{{}}', 1);
         UPDATE session_temporal_generations SET state = 'ready', ready_at = 1;
         UPDATE session_temporal_generations SET state = 'active', activated_at = 2;
         INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         ) VALUES ('seek-receipt', 'seek-sanitizer', 'sha256:seek', '{{}}');
         INSERT INTO observations (
            observation_id, payload_digest, receipt_id, observation_json,
            committed_cursor_json
         ) VALUES (
            'seek-observation', 'sha256:seek', 'seek-receipt',
            '{{\"identity\":{{\"source\":{{\"provider\":\"claude\"}}}}}}', '{{}}'
         );
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES
            ('seek-anchor-first', '{{}}', '{{}}', 'fixture'),
            ('seek-anchor-last', '{{}}', '{{}}', 'fixture'),
            ('seek-span-anchor', '{{}}', '{{}}', 'fixture'),
            ('seek-unrelated-anchor', '{{}}', '{{}}', 'fixture'),
            ('seek-unrelated-object', '{{}}', '{{}}', 'fixture');
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            source_sequence, source_provider, projection_output_ordinal,
            retrieval_anchor_id, message_id, copied_from_anchor_ids_json, role,
            knowledge_at, valid_time_json, evidence_json, sanitized_content_digest,
            sanitized_content_bytes, index_text
         ) VALUES
            ('{SESSION}', 1, 'seek-first', 'seek-observation', 1, 'claude', 0,
             'seek-anchor-first', 'seek-message-first', '[]', 'user', 1,
             '{{\"kind\":\"unknown\"}}', '{EVIDENCE}',
             '0000000000000000000000000000000000000000000000000000000000000000', 5, 'first'),
            ('{SESSION}', 1, 'seek-last', 'seek-observation', 2, 'claude', 0,
             'seek-anchor-last', 'seek-message-last', '[]', 'user', 2,
             '{{\"kind\":\"unknown\"}}', '{EVIDENCE}',
             '0000000000000000000000000000000000000000000000000000000000000000', 4, 'last');
         INSERT INTO session_derived_evidence (
            session_id, generation, evidence_kind, evidence_id,
            retrieval_anchor_id, thread_id, first_occurrence_id, last_occurrence_id,
            algorithm_version, configuration_digest, member_count, member_digest,
            evidence_json
         ) VALUES (
            '{SESSION}', 1, 'span', 'seek-span', 'seek-span-anchor', NULL,
            'seek-first', 'seek-last', 'span-v1', 'sha256:configuration', 2,
            'sha256:members', '{{}}'
         );
         INSERT INTO session_assertions (
            session_id, generation, assertion_id, assertion_kind, subject_anchor_id,
            object_anchor_id, knowledge_at, valid_time_json, evidence_json
         ) VALUES (
            '{SESSION}', 1, 'seek-assertion', 'supports', 'seek-anchor-first',
            'seek-unrelated-object', 3, '{{\"kind\":\"unknown\"}}', '{{}}'
         );
         INSERT INTO session_summary_nodes (
            summary_id, session_id, provider, conversation_id, depth, summary_anchor_id,
            summary_text, summary_hash, summary_token_count, source_token_count,
            source_horizon_json, created_at
         ) VALUES (
            'seek-summary', '{SESSION}', 'claude', '{SESSION}', 0, 'seek-anchor-last',
            'seek summary', 'sha256:summary', 1, 1, '{{}}', 1
         );
         INSERT INTO session_summary_availability (
            session_id, generation, summary_id, availability, source_horizon_json,
            reason, checked_at
         ) VALUES ('{SESSION}', 1, 'seek-summary', 'available', '{{}}', NULL, 1);"
    )
}

/// Rows of the probed session that no requested key matches: occurrences,
/// assertions, summaries and summary availability, numbered `start..end`.
fn unrelated_rows_sql(start: usize, end: usize) -> String {
    let last = end - 1;
    format!(
        "WITH RECURSIVE sequence(value) AS (
            VALUES({start}) UNION ALL SELECT value + 1 FROM sequence WHERE value < {last}
         )
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            source_sequence, source_provider, projection_output_ordinal,
            retrieval_anchor_id, message_id, copied_from_anchor_ids_json, role,
            knowledge_at, valid_time_json, evidence_json, sanitized_content_digest,
            sanitized_content_bytes, index_text
         )
         SELECT '{SESSION}', 1, printf('seek-unrelated-%06d', value), 'seek-observation',
                value + 10, 'claude', 0, 'seek-unrelated-anchor',
                printf('seek-unrelated-message-%06d', value), '[]', 'assistant', value + 10,
                '{{\"kind\":\"unknown\"}}', '{{}}',
                '0000000000000000000000000000000000000000000000000000000000000000', 9,
                'unrelated'
         FROM sequence;
         WITH RECURSIVE sequence(value) AS (
            VALUES({start}) UNION ALL SELECT value + 1 FROM sequence WHERE value < {last}
         )
         INSERT INTO session_assertions (
            session_id, generation, assertion_id, assertion_kind, subject_anchor_id,
            object_anchor_id, knowledge_at, valid_time_json, evidence_json
         )
         SELECT '{SESSION}', 1, printf('seek-unrelated-assertion-%06d', value), 'supports',
                'seek-unrelated-anchor', 'seek-unrelated-object', value + 10,
                '{{\"kind\":\"unknown\"}}', '{{}}'
         FROM sequence;
         WITH RECURSIVE sequence(value) AS (
            VALUES({start}) UNION ALL SELECT value + 1 FROM sequence WHERE value < {last}
         )
         INSERT INTO session_summary_nodes (
            summary_id, session_id, provider, conversation_id, depth, summary_anchor_id,
            summary_text, summary_hash, summary_token_count, source_token_count,
            source_horizon_json, created_at
         )
         SELECT printf('seek-unrelated-summary-%06d', value), '{SESSION}', 'claude',
                '{SESSION}', 0, 'seek-unrelated-anchor', 'unrelated summary',
                'sha256:unrelated', 1, 1, '{{}}', value + 10
         FROM sequence;
         INSERT INTO session_summary_availability (
            session_id, generation, summary_id, availability, source_horizon_json,
            reason, checked_at
         )
         SELECT '{SESSION}', 1, summary_id, 'available', '{{}}', NULL, 1
         FROM session_summary_nodes
         WHERE summary_id >= printf('seek-unrelated-summary-%06d', {start})
           AND summary_id <= printf('seek-unrelated-summary-%06d', {last});"
    )
}

/// One requested-key statement: its SQL, its bound values, and the keys it
/// must return in `column`, sorted.
struct Probe {
    name: &'static str,
    sql: String,
    params: Vec<tracedecay_sql::types::Value>,
    column: usize,
    expected: Vec<&'static str>,
}

fn text(value: &str) -> tracedecay_sql::types::Value {
    tracedecay_sql::types::Value::Text(value.to_string())
}

fn integer(value: i64) -> tracedecay_sql::types::Value {
    tracedecay_sql::types::Value::Integer(value)
}

fn span_record_probe() -> Probe {
    let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let mut candidate = candidate_for_anchor("seek-span-anchor");
    candidate.channel = CandidateChannel::Span;
    candidate.retriever_record_id = "seek-span".to_string();
    let request = record_request();
    let query = build_record_query_with_relations(
        snapshot.retrieval_scope(),
        &snapshot,
        &[candidate],
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        request.page_item_limit().saturating_add(1),
        &request,
        &RecordRelationBatch::empty(),
    )
    .expect("span record query");
    let params = query
        .params
        .into_iter()
        .map(|value| match value {
            SqlValue::Null => tracedecay_sql::types::Value::Null,
            SqlValue::Integer(value) => integer(value),
            SqlValue::Real(value) => tracedecay_sql::types::Value::Real(value),
            SqlValue::Text(value) => tracedecay_sql::types::Value::Text(value),
            SqlValue::Blob(value) => tracedecay_sql::types::Value::Blob(value),
        })
        .collect();
    Probe {
        name: "span boundary records",
        sql: query.sql,
        params,
        column: 2,
        expected: vec!["seek-first", "seek-last"],
    }
}

fn probes() -> Vec<Probe> {
    vec![
        Probe {
            name: "materialized message occurrences",
            sql: crate::operations::message_anchor::MATERIALIZED_OCCURRENCES_SQL.to_string(),
            params: vec![
                text(SESSION),
                integer(1),
                text(r#"["seek-message-last","seek-message-first","seek-message-first"]"#),
            ],
            column: 0,
            expected: vec!["seek-message-first", "seek-message-last"],
        },
        Probe {
            name: "occurrence-anchor winners",
            sql: crate::projection::persist::occurrence_anchor_winners_sql(),
            params: vec![
                text(SESSION),
                integer(1),
                text(r#"["seek-anchor-first","seek-anchor-last"]"#),
            ],
            column: 0,
            expected: Vec::new(),
        },
        Probe {
            name: "copy endpoints",
            sql: crate::projection::persist::COPY_ENDPOINTS_SQL.to_string(),
            params: vec![
                text(SESSION),
                integer(1),
                text("seek-last"),
                text("seek-first"),
            ],
            column: 0,
            expected: vec!["seek-first", "seek-last"],
        },
        Probe {
            name: "anchor assertions",
            sql: crate::expand::ANCHOR_ASSERTIONS_SQL.to_string(),
            params: vec![
                text(SESSION),
                integer(1),
                text(r#"["seek-anchor-first","seek-anchor-last"]"#),
                text("forensic"),
                integer(i64::MAX),
            ],
            column: 0,
            expected: vec!["seek-assertion"],
        },
        Probe {
            name: "summary anchors",
            sql: crate::expand::SUMMARY_ANCHORS_SQL.to_string(),
            params: vec![text(SESSION), text(r#"["seek-summary","missing-summary"]"#)],
            column: 0,
            expected: vec!["seek-summary"],
        },
        Probe {
            name: "source summary availability",
            sql: crate::operations::sources::SOURCE_SUMMARY_AVAILABILITY_SQL.to_string(),
            params: vec![
                text(SESSION),
                integer(1),
                text(r#"["seek-summary","missing-summary"]"#),
            ],
            column: 0,
            expected: vec!["seek-summary"],
        },
        span_record_probe(),
    ]
}

fn returned(connection: &tracedecay_sql::Connection, probe: &Probe) -> Vec<String> {
    let mut statement = connection.prepare(&probe.sql).expect("prepare probe");
    let mut rows = statement
        .query(tracedecay_sql::params_from_iter(probe.params.iter()))
        .expect("run probe");
    let mut values = Vec::new();
    while let Some(row) = rows.next().expect("probe row") {
        values.push(row.get::<_, String>(probe.column).expect("returned key"));
    }
    values.sort();
    values
}

fn current_occurrence_winners(connection: &tracedecay_sql::Connection) -> Vec<(String, String)> {
    let mut statement = connection
        .prepare(
            "SELECT entity_id, current_occurrence_id FROM session_current_entities
             WHERE session_id = ?1 AND entity_kind = 'occurrence_anchor'
             ORDER BY entity_id",
        )
        .expect("prepare winners");
    statement
        .query_map([SESSION], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("read winners")
        .map(|row| row.expect("winner row"))
        .collect()
}

#[tokio::test]
async fn requested_key_lookups_seek_past_unrelated_session_rows() {
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("sessions.db");
    let (database, owner) = open_registered_test_database_fixture(
        &database_path,
        TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await
    .expect("registered schema");
    drop(owner);
    drop(database);

    let raw = tracedecay_sql::Connection::open(&database_path).expect("open measured store");
    raw.execute_batch(&format!(
        "BEGIN; {} {} ANALYZE; COMMIT;",
        target_rows_sql(),
        unrelated_rows_sql(0, BASE_UNRELATED)
    ))
    .expect("seed the probed session");
    let probes = probes();
    for probe in &probes {
        assert_eq!(
            returned(&raw, probe),
            probe.expected,
            "{} must return exactly its requested rows",
            probe.name
        );
    }
    let winners = vec![
        ("seek-anchor-first".to_string(), "seek-first".to_string()),
        ("seek-anchor-last".to_string(), "seek-last".to_string()),
    ];
    assert_eq!(
        current_occurrence_winners(&raw),
        winners,
        "the winners statement must record exactly the requested anchors"
    );
    let base = probes
        .iter()
        .map(|probe| {
            crate::test_support::statement_vm_steps(
                &raw,
                &probe.sql,
                tracedecay_sql::params_from_iter(probe.params.iter()),
            )
        })
        .collect::<Vec<_>>();

    raw.execute_batch(&format!(
        "BEGIN; {} ANALYZE; COMMIT;",
        unrelated_rows_sql(BASE_UNRELATED, GROWN_UNRELATED)
    ))
    .expect("grow the probed session eightfold");
    for (probe, base) in probes.iter().zip(base) {
        assert_eq!(
            returned(&raw, probe),
            probe.expected,
            "{} must ignore the unrelated rows",
            probe.name
        );
        let grown = crate::test_support::statement_vm_steps(
            &raw,
            &probe.sql,
            tracedecay_sql::params_from_iter(probe.params.iter()),
        );
        assert!(
            grown <= 2 * base,
            "{} must seek its requested keys, not read the session: \
             {base} -> {grown} VM steps over an eightfold session",
            probe.name
        );
    }
    assert_eq!(current_occurrence_winners(&raw), winners);
}
