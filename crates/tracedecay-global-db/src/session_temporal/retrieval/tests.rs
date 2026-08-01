use tempfile::tempdir;
use tracedecay_domain::{
    MAX_OBSERVATION_RECORD_BYTES, RetrievalAnchorId, RetrievalGrainV1, SessionId, TemporalModeV1,
    TemporalValidityV1, UtcMicros,
};

use super::candidates::*;
use super::cursors::*;
use super::queries::*;
use super::records::*;
use super::*;
use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::db::engine::{
    Connection, Executor, ReadSnapshot, TestConnection, Value as SqlValue,
};
use tracedecay_temporal_query::candidates::CandidateChannel;
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, PageRequest, TemporalAuthorizedRoot, TemporalExecutionSnapshot,
    TemporalRecord, TemporalRetrievalScope, TemporalSnapshotRequest, TemporalWatermarks,
};
use tracedecay_temporal_query::ranking::RankingCandidate;
use tracedecay_temporal_query::resolution::{SummarySourceState, ValidatedAuthorization};

fn normalize_plan_detail(detail: &str) -> String {
    detail
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn snapshot(generation: u64) -> TemporalExecutionSnapshot {
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            SessionId::new("session-snapshot").expect("session"),
            digest('1'),
            digest('2'),
            digest('3'),
            TemporalModeV1::Current,
            RetrievalGrainV1::Session,
        )
        .expect("request"),
        TemporalWatermarks {
            generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('4'))
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn scoped_snapshot(generation: u64, provider: Option<&str>) -> TemporalExecutionSnapshot {
    scoped_snapshot_with_mode(generation, provider, TemporalModeV1::Current)
}

fn scoped_snapshot_with_mode(
    generation: u64,
    provider: Option<&str>,
    mode: TemporalModeV1,
) -> TemporalExecutionSnapshot {
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            SessionId::new("session-snapshot").expect("session"),
            digest('1'),
            digest('2'),
            digest('3'),
            mode,
            RetrievalGrainV1::Session,
        )
        .expect("request")
        .with_provider_scope(provider.map(str::to_string))
        .expect("provider scope"),
        TemporalWatermarks {
            generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('4'))
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn root_snapshot_with_mode(
    generation: u64,
    provider: Option<&str>,
    mode: TemporalModeV1,
) -> TemporalExecutionSnapshot {
    let request = TemporalSnapshotRequest::new(
        SessionId::new("session-snapshot").expect("session"),
        digest('1'),
        digest('2'),
        digest('3'),
        mode,
        RetrievalGrainV1::Session,
    )
    .expect("request")
    .with_authorized_root(
        TemporalAuthorizedRoot::profile("profile-1", "store-1", "root-1").expect("profile root"),
    )
    .expect("authorized root")
    .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot)
    .with_provider_scope(provider.map(str::to_string))
    .expect("provider scope");
    TemporalExecutionSnapshot::new_authorized(
        request,
        TemporalWatermarks {
            generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('4'))
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn record_request() -> PageRequest {
    PageRequest::for_test(32, 64 * 1024, 8 * 1024, 32, 512)
}

fn candidate_for_anchor(anchor_id: &str) -> RankingCandidate {
    RankingCandidate {
        stable_id: "exact:occurrence-1".to_string(),
        anchor_id: RetrievalAnchorId::new(anchor_id).expect("anchor"),
        retriever_record_id: "occurrence-1".to_string(),
        channel: CandidateChannel::ExactMessage,
        raw_score: 1_000,
        knowledge_at_micros: 1,
        logical_message: Some("message-1".to_string()),
        turn: None,
        session: Some("session-snapshot".to_string()),
        source: Some("claude".to_string()),
        evidence_role: Some("user".to_string()),
        exact_ranges: Vec::new(),
    }
}

struct RegisteredTemporalRead {
    read: ReadSnapshot,
}

impl RegisteredTemporalRead {
    fn adapter(&self) -> GlobalDbTemporalReadPort<'_> {
        GlobalDbTemporalReadPort::new_registered(&self.read)
    }

    async fn record_kinds(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        candidate: RankingCandidate,
        request: &PageRequest,
    ) -> Vec<String> {
        let query = build_record_query(
            snapshot.retrieval_scope(),
            snapshot,
            &[candidate],
            0,
            &RecordCursor {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            },
            request.page_item_limit().saturating_add(1),
            request,
        )
        .expect("record query");
        let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
            &self.read,
            &query.sql,
            query.params,
        )
        .await
        .expect("record rows");
        let mut kinds = Vec::new();
        while let Some(row) = rows.next().await.expect("record row") {
            kinds.push(row.get(3).expect("record kind"));
        }
        kinds
    }

    async fn records(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        candidate: RankingCandidate,
        request: &PageRequest,
    ) -> Vec<TemporalRecord> {
        let query = build_record_query(
            snapshot.retrieval_scope(),
            snapshot,
            &[candidate],
            0,
            &RecordCursor {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            },
            request.page_item_limit().saturating_add(1),
            request,
        )
        .expect("record query");
        let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
            &self.read,
            &query.sql,
            query.params,
        )
        .await
        .expect("record rows");
        let mut records = Vec::new();
        while let Some(row) = rows.next().await.expect("record row") {
            records.push(temporal_record_from_row(&row).expect("typed temporal record"));
        }
        records
    }

    async fn explain_record_query(&self, query: RecordQuery) -> Vec<String> {
        let explain = format!("EXPLAIN QUERY PLAN {}", query.sql);
        let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
            &self.read,
            &explain,
            query.params,
        )
        .await
        .expect("record query must parse and plan");
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.expect("plan row") {
            let detail: String = row.get(3).expect("record plan detail");
            details.push(normalize_plan_detail(&detail));
        }
        details
    }

    async fn text_column(&self, sql: &str, params: Vec<SqlValue>, column: i32) -> Vec<String> {
        let mut rows =
            tracedecay_runtime_core::db::engine::QueryExecutor::query(&self.read, sql, params)
                .await
                .expect("query must execute");
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.expect("query row") {
            values.push(row.get(column).expect("text column"));
        }
        values
    }

    async fn optional_text_column(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        column: i32,
    ) -> Vec<Option<String>> {
        let mut rows =
            tracedecay_runtime_core::db::engine::QueryExecutor::query(&self.read, sql, params)
                .await
                .expect("query must execute");
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.expect("query row") {
            values.push(row.get(column).expect("optional text column"));
        }
        values
    }

    async fn explain_query_plan(&self, sql: &str, params: Vec<SqlValue>) -> Vec<String> {
        let explain = format!("EXPLAIN QUERY PLAN {sql}");
        let mut rows =
            tracedecay_runtime_core::db::engine::QueryExecutor::query(&self.read, &explain, params)
                .await
                .expect("query must plan");
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.expect("plan row") {
            let detail: String = row.get(3).expect("plan detail");
            details.push(normalize_plan_detail(&detail));
        }
        details
    }
}

impl HostAdmissionTestRuntimeV1 {
    async fn retrieval_read_for_test(&self) -> RegisteredTemporalRead {
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        RegisteredTemporalRead {
            read: database.read_snapshot().await.expect("read snapshot"),
        }
    }

    async fn seed_candidate_query_fixture_for_test(&self) {
        self.activate_temporal_generation_for_retrieval_test("session-plan-inside", 1)
            .await;
        self.activate_temporal_generation_for_retrieval_test("session-plan-outside", 1)
            .await;
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        Executor::execute_batch(
            &database
                .writer_connection()
                .expect("registered profile writer"),
            "INSERT INTO sessions (
                provider, session_id, project_key, project_path
             ) VALUES
                ('claude', 'session-plan-inside', 'user', '/candidate-plan'),
                ('claude', 'session-plan-outside', 'project-outside', '/outside');
             INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES
                ('receipt-plan-inside', 'fixture', 'sha256:plan-inside', '{}'),
                ('receipt-plan-outside', 'fixture', 'sha256:plan-outside', '{}');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES
                (
                    'observation-plan-inside', 'sha256:plan-inside',
                    'receipt-plan-inside',
                    '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
                ),
                (
                    'observation-plan-outside', 'sha256:plan-outside',
                    'receipt-plan-outside',
                    '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
                );
             INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                (
                    'anchor-plan-inside', '{}', '{\"kind\":\"profile\"}', 'fixture'
                ),
                (
                    'anchor-plan-inside-old', '{}', '{\"kind\":\"profile\"}', 'fixture'
                ),
                (
                    'anchor-plan-inside-last', '{}', '{\"kind\":\"profile\"}', 'fixture'
                ),
                (
                    'anchor-plan-summary', '{}', '{\"kind\":\"profile\"}', 'fixture'
                ),
                (
                    'anchor-plan-summary-old', '{}', '{\"kind\":\"profile\"}', 'fixture'
                ),
                (
                    'anchor-plan-derived', '{}', '{\"kind\":\"profile\"}', 'fixture'
                ),
                (
                    'anchor-plan-derived-old', '{}', '{\"kind\":\"profile\"}', 'fixture'
                ),
                (
                    'anchor-plan-outside', '{}',
                    '{\"kind\":\"project\",\"project_id\":\"project-outside\"}', 'fixture'
                ),
                (
                    'anchor-plan-summary-outside', '{}',
                    '{\"kind\":\"project\",\"project_id\":\"project-outside\"}', 'fixture'
                ),
                (
                    'anchor-plan-derived-outside', '{}',
                    '{\"kind\":\"project\",\"project_id\":\"project-outside\"}', 'fixture'
                );
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, message_id, turn_id,
                role, knowledge_at, valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES
                (
                    'session-plan-inside', 1, 'occurrence-plan-inside',
                    'observation-plan-inside', 0, 'anchor-plan-inside',
                    'message-plan-inside', 'turn-plan-inside', 'user', 20,
                    '{\"kind\":\"unknown\"}', '{}',
                    'needle candidate derived needle inside',
                    'needle candidate derived needle inside'
                ),
                (
                    'session-plan-inside', 1, 'occurrence-plan-inside-old',
                    'observation-plan-inside', 1, 'anchor-plan-inside-old',
                    'message-plan-inside-old', 'turn-plan-inside-old', 'assistant', 10,
                    '{\"kind\":\"unknown\"}', '{}',
                    'derived needle older member', 'derived needle older member'
                ),
                (
                    'session-plan-inside', 1, 'occurrence-plan-inside-last',
                    'observation-plan-inside', 2, 'anchor-plan-inside-last',
                    'message-plan-inside-last', 'turn-plan-inside-last', 'assistant', 5,
                    '{\"kind\":\"unknown\"}', '{}',
                    'derived needle last member', 'derived needle last member'
                ),
                (
                    'session-plan-outside', 1, 'occurrence-plan-outside',
                    'observation-plan-outside', 0, 'anchor-plan-outside',
                    'message-plan-outside', 'turn-plan-outside', 'user', 30,
                    '{\"kind\":\"unknown\"}', '{}',
                    'needle candidate derived needle outside',
                    'needle candidate derived needle outside'
                );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES
                (
                    'summary-plan-inside', 'session-plan-inside', 'anchor-plan-summary',
                    'needle summary inside newest', 'needle summary inside newest', '{}',
                    '{\"provider\":\"claude\"}', 25
                ),
                (
                    'summary-plan-inside-old', 'session-plan-inside',
                    'anchor-plan-summary-old',
                    'needle summary inside older', 'needle summary inside older', '{}',
                    '{\"provider\":\"claude\"}', 15
                ),
                (
                    'summary-plan-outside', 'session-plan-outside',
                    'anchor-plan-summary-outside',
                    'needle summary outside', 'needle summary outside', '{}',
                    '{\"provider\":\"claude\"}', 35
                );
             INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES
                (
                    'summary-plan-inside', 0, 'anchor', 'anchor-plan-inside', NULL
                ),
                (
                    'summary-plan-inside-old', 0, 'anchor',
                    'anchor-plan-inside-old', NULL
                ),
                (
                    'summary-plan-outside', 0, 'anchor', 'anchor-plan-outside', NULL
                );
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES
                (
                    'session-plan-inside', 1, 'summary-plan-inside', 'available',
                    '{}', NULL, 25
                ),
                (
                    'session-plan-inside', 1, 'summary-plan-inside-old', 'available',
                    '{}', NULL, 15
                ),
                (
                    'session-plan-outside', 1, 'summary-plan-outside', 'available',
                    '{}', NULL, 35
                );
             INSERT INTO session_derived_evidence (
                session_id, generation, evidence_kind, evidence_id,
                retrieval_anchor_id, thread_id, first_occurrence_id, last_occurrence_id,
                algorithm_version, configuration_digest, member_count, member_digest,
                evidence_json
             ) VALUES
                (
                    'session-plan-inside', 1, 'span', 'derived-plan-inside',
                    'anchor-plan-derived', NULL,
                    'occurrence-plan-inside', 'occurrence-plan-inside',
                    'fixture-v1', 'sha256:derived-inside', 1,
                    'sha256:derived-inside-members', '{}'
                ),
                (
                    'session-plan-inside', 1, 'span', 'derived-plan-inside-old',
                    'anchor-plan-derived-old', NULL,
                    'occurrence-plan-inside-old', 'occurrence-plan-inside-last',
                    'fixture-v1', 'sha256:derived-inside-old', 2,
                    'sha256:derived-inside-old-members', '{}'
                ),
                (
                    'session-plan-outside', 1, 'span', 'derived-plan-outside',
                    'anchor-plan-derived-outside', NULL,
                    'occurrence-plan-outside', 'occurrence-plan-outside',
                    'fixture-v1', 'sha256:derived-outside', 1,
                    'sha256:derived-outside-members', '{}'
                );
             INSERT INTO session_derived_evidence_members (
                session_id, generation, evidence_kind, evidence_id,
                ordinal, occurrence_id, member_role
             ) VALUES
                (
                    'session-plan-inside', 1, 'span', 'derived-plan-inside',
                    0, 'occurrence-plan-inside', 'first'
                ),
                (
                    'session-plan-inside', 1, 'span', 'derived-plan-inside-old',
                    0, 'occurrence-plan-inside-old', 'first'
                ),
                (
                    'session-plan-inside', 1, 'span', 'derived-plan-inside-old',
                    1, 'occurrence-plan-inside-last', 'last'
                ),
                (
                    'session-plan-outside', 1, 'span', 'derived-plan-outside',
                    0, 'occurrence-plan-outside', 'first'
                );",
        )
        .await
        .expect("candidate query fixture");
    }

    async fn activate_temporal_generation_for_retrieval_test(
        &self,
        session_id: &str,
        generation: u64,
    ) {
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let writer = database
            .writer_connection()
            .expect("registered profile writer");
        let frozen = serde_json::json!({
            "active_generation": generation,
            "cursor_key": null,
            "projection_frontier": 0,
            "source_frontier": 0,
            "summary_frontier": 0
        })
        .to_string();
        let generation = i64::try_from(generation).expect("generation");
        Executor::execute(
            &writer,
            "INSERT INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json, created_at,
                ready_at, activated_at, completed_at
             ) VALUES (?1, ?2, 'building', ?3, ?2, NULL, NULL, NULL)",
            (session_id, generation, frozen.as_str()),
        )
        .await
        .expect("building generation");
        Executor::execute(
            &writer,
            "UPDATE session_temporal_generations
             SET state = 'ready', ready_at = generation
             WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
            (session_id, generation),
        )
        .await
        .expect("ready generation");
        Executor::execute(
            &writer,
            "UPDATE session_temporal_generations
             SET state = 'superseded', completed_at = ?1
             WHERE session_id = ?2
               AND generation <> ?1
               AND state = 'active'",
            (generation, session_id),
        )
        .await
        .expect("supersede prior active generation");
        Executor::execute(
            &writer,
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = generation
             WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
            (session_id, generation),
        )
        .await
        .expect("activate generation");

        let mut rows = writer
            .query(
                "SELECT frozen_watermarks_json
                 FROM session_temporal_generations
                 WHERE session_id = ?1 AND generation = ?2
                 LIMIT 1",
                (session_id, generation),
            )
            .await
            .expect("query frozen watermarks");
        let encoded: String = rows
            .next()
            .await
            .expect("row")
            .expect("generation row")
            .get(0)
            .expect("frozen_watermarks_json");
        assert_eq!(
            encoded, frozen,
            "legal building→ready→active transitions must not mutate frozen_watermarks_json"
        );
    }

    async fn seed_cross_session_record_fixture_for_test(&self) {
        self.activate_temporal_generation_for_retrieval_test("session-a", 1)
            .await;
        self.activate_temporal_generation_for_retrieval_test("session-b", 1)
            .await;
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        Executor::execute_batch(
            &database
                .writer_connection()
                .expect("registered profile writer"),
            "INSERT INTO sessions (
                provider, session_id, project_key, project_path
             ) VALUES
                ('claude', 'session-a', 'user', '/root-record-test'),
                ('claude', 'session-b', 'user', '/root-record-test');
             INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES ('receipt-1', 'fixture', 'sha256:fixture', '{}');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-shared', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                ('same-anchor', '{}', '{}', 'fixture'),
                ('source-anchor-b', '{}', '{}', 'fixture'),
                ('other-anchor', '{}', '{}', 'fixture');
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES
                (
                    'session-a', 1, 'same-id', 'observation-shared', 0,
                    'same-anchor', 'user', 5, '{\"kind\":\"unknown\"}', '{}',
                    'same content', 'same content'
                ),
                (
                    'session-b', 1, 'same-id', 'observation-shared', 0,
                    'same-anchor', 'user', 5, '{\"kind\":\"unknown\"}', '{}',
                    'same content', 'same content'
                ),
                (
                    'session-b', 1, 'source-b', 'observation-shared', 1,
                    'source-anchor-b', 'user', 4, '{\"kind\":\"unknown\"}', '{}',
                    'source', 'source'
                );
             INSERT INTO session_logical_copy_edges (
                session_id, generation, occurrence_id, copied_from_occurrence_id,
                proof_json, knowledge_at, valid_time_json, created_at
             ) VALUES (
                'session-b', 1, 'same-id', 'source-b', '{}', 5,
                '{\"kind\":\"unknown\"}', 5
             );
             INSERT INTO session_assertions (
                session_id, generation, assertion_id, assertion_kind,
                subject_anchor_id, object_anchor_id, knowledge_at,
                valid_time_json, evidence_json
             ) VALUES (
                'session-b', 1, 'assertion-b', 'supports',
                'same-anchor', 'other-anchor', 5, '{\"kind\":\"unknown\"}', '{}'
             );
             INSERT INTO session_current_entities (
                session_id, generation, entity_kind, entity_id,
                current_assertion_id, current_occurrence_id, coverage_json
             ) VALUES (
                'session-b', 1, 'occurrence_anchor', 'same-anchor',
                NULL, 'same-id', '{}'
             );",
        )
        .await
        .expect("cross-session retrieval fixture");
    }

    async fn seed_derived_record_fixture_for_test(&self) {
        self.activate_temporal_generation_for_retrieval_test("session-snapshot", 1)
            .await;
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        Executor::execute_batch(
            &database
                .writer_connection()
                .expect("registered profile writer"),
            "INSERT INTO sessions (
                provider, session_id, project_key, project_path
             ) VALUES ('claude', 'session-snapshot', 'user', '/derived-record-test');
             INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES ('derived-receipt', 'fixture', 'sha256:derived', '{}');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'derived-observation', 'sha256:derived', 'derived-receipt',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                ('source-occurrence-anchor', '{}', '{}', 'fixture'),
                ('derived-span-anchor', '{}', '{}', 'fixture');
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES (
                'session-snapshot', 1,
                'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                'derived-observation', 0,
                'source-occurrence-anchor', 'user', 5, '{\"kind\":\"unknown\"}',
                '{
                    \"authority\":\"provider_native\",
                    \"evidence_class\":\"provider_declared\",
                    \"source_anchor_id\":\"source-evidence-anchor\",
                    \"sanitization_receipt\":{
                        \"receipt_id\":\"derived-receipt\",
                        \"sanitizer_version\":\"derived-sanitizer\"
                    }
                }',
                'derived content', 'derived content'
             );
             INSERT INTO session_derived_evidence (
                session_id, generation, evidence_kind, evidence_id,
                retrieval_anchor_id, thread_id, first_occurrence_id, last_occurrence_id,
                algorithm_version, configuration_digest, member_count, member_digest,
                evidence_json
             ) VALUES (
                'session-snapshot', 1, 'span', 'span-evidence-id',
                'derived-span-anchor', NULL,
                'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                'span-v1', 'sha256:configuration', 1, 'sha256:members', '{}'
             );
             INSERT INTO session_derived_evidence_members (
                session_id, generation, evidence_kind, evidence_id,
                ordinal, occurrence_id, member_role
             ) VALUES (
                'session-snapshot', 1, 'span', 'span-evidence-id',
                0,
                'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                'first'
             );",
        )
        .await
        .expect("derived retrieval fixture");
    }

    async fn seed_oversized_record_fixture_for_test(&self) {
        self.activate_temporal_generation_for_retrieval_test("session-snapshot", 1)
            .await;
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let writer = database
            .writer_connection()
            .expect("registered profile writer");
        Executor::execute_batch(
            &writer,
            "INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES ('receipt-1', 'fixture', 'sha256:fixture', '{}');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-1', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                ('anchor-evidence', '{}', '{}', 'fixture'),
                ('anchor-publication', '{}', '{}', 'fixture'),
                ('source-short', '{}', '{}', 'fixture'),
                ('anchor-source', '{}', '{}', 'fixture');",
        )
        .await
        .expect("oversized observation fixture");
        let oversized_json = serde_json::to_string(&"x".repeat(16 * 1024)).unwrap();
        Executor::execute(
            &writer,
            "INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES (
                'session-snapshot', 1, 'occurrence-oversized', 'observation-1',
                0, 'anchor-evidence', 'user', 1,
                '{\"kind\":\"unknown\"}', ?1, 'snippet', 'index'
             )",
            [oversized_json.clone()],
        )
        .await
        .expect("oversized occurrence fixture");
        Executor::execute(
            &writer,
            "INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-publication', 'session-snapshot', 'anchor-publication',
                'summary', 'summary', '{}', ?1, 1
             )",
            [oversized_json],
        )
        .await
        .expect("oversized publication fixture");
        Executor::execute_batch(
            &writer,
            "INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-publication', 0, 'anchor', 'source-short', NULL);
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-publication', 'available',
                '{}', NULL, 1
             );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-source', 'session-snapshot', 'anchor-source',
                'summary', 'summary', '{}', NULL, 1
             );",
        )
        .await
        .expect("oversized summary fixtures");
        let oversized_source = format!("source-{}", "y".repeat(512));
        Executor::execute(
            &writer,
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES (?1, '{}', '{}', 'fixture')",
            [oversized_source.as_str()],
        )
        .await
        .expect("oversized source anchor fixture");
        Executor::execute(
            &writer,
            "INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-source', 0, 'anchor', ?1, NULL)",
            [oversized_source],
        )
        .await
        .expect("oversized source fixture");
        Executor::execute_batch(
            &writer,
            "INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-source', 'available',
                '{}', NULL, 1
             );",
        )
        .await
        .expect("oversized summary availability");
    }

    async fn seed_summary_source_cap_fixture_for_test(&self) {
        self.activate_temporal_generation_for_retrieval_test("session-snapshot", 1)
            .await;
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let writer = database
            .writer_connection()
            .expect("registered profile writer");
        Executor::execute_batch(
            &writer,
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES ('anchor-many-sources', '{}', '{}', 'fixture');
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-many-sources', 'session-snapshot', 'anchor-many-sources',
                'summary', 'summary', '{}', NULL, 1
             );
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-many-sources', 'available',
                '{}', NULL, 1
             );",
        )
        .await
        .expect("many-source summary fixture");
        for ordinal in 0..=MAX_SUMMARY_SOURCES_PER_RECORD {
            let source_anchor = format!("source-{ordinal:03}");
            Executor::execute(
                &writer,
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', '{}', 'fixture')",
                [source_anchor.as_str()],
            )
            .await
            .expect("many-source anchor fixture");
            Executor::execute(
                &writer,
                "INSERT INTO session_summary_sources (
                    summary_id, source_ordinal, source_kind,
                    source_anchor_id, source_summary_id
                 ) VALUES ('summary-many-sources', ?1, 'anchor', ?2, NULL)",
                (i64::try_from(ordinal).unwrap(), source_anchor),
            )
            .await
            .expect("many-source edge fixture");
        }
    }

    async fn seed_provider_summary_fixture_for_test(&self) {
        self.activate_temporal_generation_for_retrieval_test("session-snapshot", 1)
            .await;
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        Executor::execute_batch(
            &database
                .writer_connection()
                .expect("registered profile writer"),
            "INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES ('receipt-1', 'fixture', 'sha256:fixture', '{}');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-claude', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                ('source-claude', '{}', '{}', 'fixture'),
                ('anchor-summary-provider', '{}', '{}', 'fixture');
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES (
                'session-snapshot', 1, 'occurrence-claude', 'observation-claude',
                0, 'source-claude', 'user', 1, '{\"kind\":\"unknown\"}',
                '{\"authority\":\"canonical\",\"evidence_class\":\"observed\",
                  \"source_anchor_id\":\"source-claude\",
                  \"sanitization_receipt\":{\"receipt_id\":\"receipt-1\"}}',
                'snippet', 'index'
             );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-provider', 'session-snapshot', 'anchor-summary-provider',
                'summary', 'summary', '{}', NULL, 1
             );
             INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-provider', 0, 'anchor', 'source-claude', NULL);
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-provider', 'available',
                '{}', NULL, 1
             );",
        )
        .await
        .expect("provider summary fixture");
    }

    async fn seed_historical_summary_successor_fixture_for_test(&self) {
        self.activate_temporal_generation_for_retrieval_test("session-snapshot", 1)
            .await;
        let database = self
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        Executor::execute_batch(
            &database
                .writer_connection()
                .expect("registered profile writer"),
            "INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES ('history-receipt', 'fixture', 'sha256:history', '{}');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'summary-history-observation', 'sha256:history', 'history-receipt',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                ('shared-summary-source', '{}', '{}', 'fixture'),
                ('historical-summary-anchor', '{}', '{}', 'fixture'),
                ('successor-summary-anchor', '{}', '{}', 'fixture');
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES
                (
                    'session-snapshot', 1, 'summary-source-at-5',
                    'summary-history-observation', 0, 'shared-summary-source',
                    'user', 5, '{\"kind\":\"known\",\"valid_at\":5}', '{}',
                    'source at 5', 'source at 5'
                ),
                (
                    'session-snapshot', 1, 'summary-source-at-10',
                    'summary-history-observation', 1, 'shared-summary-source',
                    'user', 10, '{\"kind\":\"known\",\"valid_at\":10}', '{}',
                    'source at 10', 'source at 10'
                );
             INSERT INTO session_current_entities (
                session_id, generation, entity_kind, entity_id,
                current_assertion_id, current_occurrence_id, coverage_json
             ) VALUES (
                'session-snapshot', 1, 'occurrence_anchor', 'shared-summary-source',
                NULL, 'summary-source-at-10', '{}'
             );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES
                (
                    'historical-summary', 'session-snapshot', 'historical-summary-anchor',
                    'historical', 'historical',
                    '{\"knowledge_through\":5,\"valid_through\":5}', NULL, 5
                ),
                (
                    'successor-summary', 'session-snapshot', 'successor-summary-anchor',
                    'successor', 'successor',
                    '{\"knowledge_through\":10,\"valid_through\":10}', NULL, 10
                );
             INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES
                ('historical-summary', 0, 'anchor', 'shared-summary-source', NULL),
                ('successor-summary', 0, 'anchor', 'shared-summary-source', NULL);
             INSERT INTO session_summary_successors (
                predecessor_summary_id, successor_summary_id, created_at
             ) VALUES ('historical-summary', 'successor-summary', 10);
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES
                (
                    'session-snapshot', 1, 'historical-summary', 'stale',
                    '{\"knowledge_through\":5,\"valid_through\":5}',
                    'predecessor_superseded', 10
                ),
                (
                    'session-snapshot', 1, 'successor-summary', 'available',
                    '{\"knowledge_through\":10,\"valid_through\":10}', NULL, 10
                );",
        )
        .await
        .expect("historical summary successor fixture");
    }
}

#[test]
fn adapter_contains_only_the_borrowed_engine_handle() {
    fn assert_exact_fields(adapter: &GlobalDbTemporalReadPort<'_>) {
        let GlobalDbTemporalReadPort { read: _ } = adapter;
    }

    let _ = assert_exact_fields;
    assert_eq!(
        std::mem::size_of::<GlobalDbTemporalReadPort<'static>>(),
        std::mem::size_of::<super::super::sql::TemporalSqlRead<'static>>()
    );
}

#[tokio::test]
async fn frozen_generation_survives_rotation_while_a_new_snapshot_observes_drift() {
    let dir = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime
        .activate_temporal_generation_for_retrieval_test("session-snapshot", 1)
        .await;
    let frozen_snapshot = snapshot(1);
    let frozen_read = runtime.retrieval_read_for_test().await;
    let frozen_adapter = frozen_read.adapter();
    frozen_adapter
        .validate_snapshot(&frozen_snapshot)
        .await
        .expect("generation one is frozen active");

    runtime
        .activate_temporal_generation_for_retrieval_test("session-snapshot", 2)
        .await;

    frozen_adapter
        .validate_snapshot(&frozen_snapshot)
        .await
        .expect("same read snapshot retains generation one");
    let fresh_read = runtime.retrieval_read_for_test().await;
    let fresh_adapter = fresh_read.adapter();
    assert!(
        fresh_adapter
            .validate_snapshot(&frozen_snapshot)
            .await
            .is_err()
    );
    fresh_adapter
        .validate_snapshot(&snapshot(2))
        .await
        .expect("new read snapshot sees generation two");
}

#[test]
fn candidate_and_record_cursors_are_stable_and_bounded() {
    let candidate = CandidateCursor {
        clause: 42,
        knowledge_at: 1_234_567,
        session_id: "session-b".to_string(),
        stable_id: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_string(),
    };
    let encoded = candidate.encode(256).unwrap();
    assert_eq!(CandidateCursor::decode(Some(&encoded)).unwrap(), candidate);

    let record = RecordCursor {
        candidate: 99_999,
        kind: 4,
        session_id: "session-b".to_string(),
        stable_id: "summary:17".to_string(),
    };
    let encoded = record.encode(256).unwrap();
    assert_eq!(RecordCursor::decode(Some(&encoded)).unwrap(), record);
    assert!(record.encode(8).is_err());
}

#[tokio::test]
async fn duplicate_frozen_generation_rows_fail_closed_as_not_unique() {
    let dir = tempdir().expect("temporary directory");
    let conn = TestConnection::open(&dir.path().join("ambiguous-generation.db"));
    let frozen = serde_json::json!({
        "active_generation": 1,
        "cursor_key": null,
        "projection_frontier": 0,
        "source_frontier": 0,
        "summary_frontier": 0
    })
    .to_string();
    // No primary key: the production uniqueness probe must still fail closed
    // when two matching generation rows are visible under LIMIT 2.
    conn.execute_batch(&format!(
        "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                frozen_watermarks_json TEXT NOT NULL
             );
             INSERT INTO session_temporal_generations VALUES
                ('session-snapshot', 1, 'active', '{frozen}'),
                ('session-snapshot', 1, 'active', '{frozen}');"
    ))
    .await
    .expect("ambiguous generation fixture");

    let adapter = GlobalDbTemporalReadPort::new(&conn);
    let error = adapter
        .validate_snapshot(&snapshot(1))
        .await
        .expect_err("duplicate frozen generation rows must fail closed");
    assert!(
        error
            .to_string()
            .contains("frozen generation is not unique"),
        "unexpected ambiguity error: {error:?}"
    );
}

#[test]
fn one_hundred_thousand_candidates_are_windowed_before_sql_allocation() {
    let total = 100_000usize;
    let page_items = 37usize;
    let start = 71_111usize;
    let end = bounded_window_end(total, start, page_items.saturating_add(1));
    assert_eq!(end - start, 38);
    assert!(end < total);
}

#[test]
fn mode_sql_is_shaped_without_optional_or_fallback_predicates() {
    let current = RecordModeSql::new(TemporalModeV1::Current, 9);
    assert!(current.occurrence_join.contains("session_current_entities"));
    assert!(
        current.assertion_join.is_empty(),
        "current resolution needs every assertion, including conflicts and support"
    );
    assert!(!current.occurrence_join.contains(" OR "));

    let as_of = RecordModeSql::new(
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(10),
        },
        9,
    );
    assert!(as_of.occurrence_predicate.contains("o.knowledge_at <= ?9"));
    assert!(as_of.assertion_predicate.contains("a.knowledge_at <= ?9"));
    assert!(as_of.copy_predicate.contains("e.knowledge_at <= ?9"));
    assert!(
        as_of
            .copy_predicate
            .contains("json_extract(e.valid_time_json, '$.kind') = 'known'")
    );
    assert!(
        as_of
            .copy_predicate
            .contains("json_extract(e.valid_time_json, '$.valid_at') <= ?9")
    );
    assert!(!as_of.copy_predicate.contains("e.created_at"));

    let evolution = RecordModeSql::new(TemporalModeV1::Evolution, 9);
    assert!(evolution.summary_predicate.contains("availability"));
    let forensic = RecordModeSql::new(TemporalModeV1::Forensic, 9);
    assert_eq!(forensic.summary_predicate, "1 = 1");
}

#[tokio::test]
async fn candidate_queries_return_live_rows_and_use_schema_indexes() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_candidate_query_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    let exact_params = vec![
        SqlValue::Text("session-plan-inside".to_string()),
        SqlValue::Integer(1),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text("needle candidate".to_string()),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(i64::try_from(MAX_OBSERVATION_RECORD_BYTES).expect("source byte cap")),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(EXACT_CANDIDATE_QUERY, exact_params, 0)
            .await,
        ["occurrence-plan-inside"]
    );

    let occurrence_fts_params = vec![
        SqlValue::Text("session-plan-inside".to_string()),
        SqlValue::Integer(1),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("needle candidate")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(OCCURRENCE_FTS_QUERY, occurrence_fts_params.clone(), 0)
            .await,
        ["occurrence-plan-inside"]
    );
    let occurrence_fts_plan = read
        .explain_query_plan(OCCURRENCE_FTS_QUERY, occurrence_fts_params)
        .await;
    assert!(
        occurrence_fts_plan.iter().any(|detail| {
            detail.contains("SESSION_OCCURRENCES_FTS") && detail.contains("VIRTUAL TABLE INDEX")
        }),
        "occurrence retrieval must execute through the live FTS operator: {occurrence_fts_plan:?}"
    );

    let time_params = vec![
        SqlValue::Text("session-plan-inside".to_string()),
        SqlValue::Integer(1),
        SqlValue::Text("claude".to_string()),
        SqlValue::Integer(0),
        SqlValue::Integer(100),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(TIME_CANDIDATE_QUERY, time_params.clone(), 0)
            .await,
        ["occurrence-plan-inside"]
    );
    let time_plan = read
        .explain_query_plan(TIME_CANDIDATE_QUERY, time_params)
        .await;
    assert!(
        time_plan
            .iter()
            .any(|detail| detail.contains("IDX_SESSION_OCCURRENCES_GENERATION_ORDER")),
        "session time retrieval must use the live generation-order index: {time_plan:?}"
    );

    let summary_params = vec![
        SqlValue::Text("session-plan-inside".to_string()),
        SqlValue::Integer(1),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("needle summary")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(SUMMARY_CANDIDATE_QUERY, summary_params.clone(), 0)
            .await,
        ["summary-plan-inside", "summary-plan-inside-old"]
    );
    let summary_plan = read
        .explain_query_plan(SUMMARY_CANDIDATE_QUERY, summary_params)
        .await;
    assert!(
        summary_plan.iter().any(|detail| {
            detail.contains("SESSION_SUMMARY_NODES_FTS") && detail.contains("VIRTUAL TABLE INDEX")
        }),
        "summary retrieval must execute through the live FTS operator: {summary_plan:?}"
    );

    let root_fts_params = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Null,
        SqlValue::Text(fts_phrase("needle candidate")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(128),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(ROOT_OCCURRENCE_FTS_QUERY, root_fts_params.clone(), 0)
            .await,
        ["occurrence-plan-inside"],
        "root retrieval must exclude the populated out-of-root session"
    );
    let root_fts_plan = read
        .explain_query_plan(ROOT_OCCURRENCE_FTS_QUERY, root_fts_params)
        .await;
    assert!(
        root_fts_plan.iter().any(|detail| {
            detail.contains("SESSION_OCCURRENCES_FTS") && detail.contains("VIRTUAL TABLE INDEX")
        }),
        "root retrieval must execute one live FTS store query: {root_fts_plan:?}"
    );

    let root_time_params = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Null,
        SqlValue::Integer(0),
        SqlValue::Integer(100),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(128),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(ROOT_TIME_CANDIDATE_QUERY, root_time_params.clone(), 0)
            .await,
        ["occurrence-plan-inside"],
        "root time retrieval must exclude the populated out-of-root session"
    );
    let root_time_plan = read
        .explain_query_plan(ROOT_TIME_CANDIDATE_QUERY, root_time_params)
        .await;
    assert!(
        root_time_plan
            .iter()
            .any(|detail| detail.contains("IDX_SESSION_OCCURRENCES_ROOT_GENERATION_ORDER")),
        "root time retrieval must use the live root generation-order index: {root_time_plan:?}"
    );
    assert!(
        root_time_plan
            .iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE")),
        "root time retrieval must preserve index order: {root_time_plan:?}"
    );
}

#[tokio::test]
async fn summary_and_derived_candidate_queries_enforce_live_boundaries_and_plans() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_candidate_query_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;

    let root_summary_params = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("needle summary")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(128),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(ROOT_SUMMARY_CANDIDATE_QUERY, root_summary_params.clone(), 0)
            .await,
        ["summary-plan-inside", "summary-plan-inside-old"],
        "root summary retrieval must be ordered and exclude the populated out-of-root summary"
    );
    let root_summary_plan = read
        .explain_query_plan(ROOT_SUMMARY_CANDIDATE_QUERY, root_summary_params)
        .await;
    assert!(
        root_summary_plan.iter().any(|detail| {
            detail.contains("SESSION_SUMMARY_NODES_FTS") && detail.contains("VIRTUAL TABLE INDEX")
        }),
        "root summaries must use the live summary FTS operator: {root_summary_plan:?}"
    );
    assert!(
        root_summary_plan
            .iter()
            .any(|detail| detail.contains("IDX_SESSION_SUMMARY_NODES_ROOT_CREATED_ORDER")),
        "root summaries must use the live root ordering index: {root_summary_plan:?}"
    );
    let root_summary_wrong_provider = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Text("codex".to_string()),
        SqlValue::Text(fts_phrase("needle summary")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(128),
        SqlValue::Integer(10),
    ];
    assert!(
        read.text_column(ROOT_SUMMARY_CANDIDATE_QUERY, root_summary_wrong_provider, 0)
            .await
            .is_empty(),
        "root summaries must fail closed for a provider without retained source evidence"
    );
    let root_summary_missing_phrase = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("absent summary phrase")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(128),
        SqlValue::Integer(1_024),
        SqlValue::Integer(128),
        SqlValue::Integer(10),
    ];
    assert!(
        read.text_column(ROOT_SUMMARY_CANDIDATE_QUERY, root_summary_missing_phrase, 0)
            .await
            .is_empty(),
        "root summary retrieval must require a real FTS match"
    );

    let derived_params = vec![
        SqlValue::Text("session-plan-inside".to_string()),
        SqlValue::Integer(1),
        SqlValue::Text("span".to_string()),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("derived needle")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(DERIVED_CANDIDATE_QUERY, derived_params.clone(), 0)
            .await,
        ["derived-plan-inside", "derived-plan-inside-old"]
    );
    assert_eq!(
        read.optional_text_column(DERIVED_CANDIDATE_QUERY, derived_params.clone(), 3)
            .await,
        [Some("message-plan-inside".to_string()), None],
        "only singleton derived evidence may inherit its member logical message"
    );
    let derived_plan = read
        .explain_query_plan(DERIVED_CANDIDATE_QUERY, derived_params)
        .await;
    assert!(
        derived_plan.iter().any(|detail| {
            detail.contains("SESSION_OCCURRENCES_FTS") && detail.contains("VIRTUAL TABLE INDEX")
        }),
        "derived retrieval must search members through the live occurrence FTS operator: {derived_plan:?}"
    );
    assert!(
        derived_plan
            .iter()
            .any(|detail| detail.contains("IDX_SESSION_DERIVED_EVIDENCE_SCOPE_ORDER")),
        "derived retrieval must use the live evidence scope index: {derived_plan:?}"
    );
    let derived_wrong_provider = vec![
        SqlValue::Text("session-plan-inside".to_string()),
        SqlValue::Integer(1),
        SqlValue::Text("span".to_string()),
        SqlValue::Text("codex".to_string()),
        SqlValue::Text(fts_phrase("derived needle")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Integer(10),
    ];
    assert!(
        read.text_column(DERIVED_CANDIDATE_QUERY, derived_wrong_provider, 0)
            .await
            .is_empty(),
        "derived candidates must fail closed for the wrong provider"
    );
    let derived_missing_phrase = vec![
        SqlValue::Text("session-plan-inside".to_string()),
        SqlValue::Integer(1),
        SqlValue::Text("span".to_string()),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("absent derived phrase")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Integer(10),
    ];
    assert!(
        read.text_column(DERIVED_CANDIDATE_QUERY, derived_missing_phrase, 0)
            .await
            .is_empty(),
        "derived candidates must require an FTS-matching member"
    );

    let root_derived_params = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Text("span".to_string()),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("derived needle")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(10),
    ];
    assert_eq!(
        read.text_column(ROOT_DERIVED_CANDIDATE_QUERY, root_derived_params.clone(), 0)
            .await,
        ["derived-plan-inside", "derived-plan-inside-old"],
        "root derived retrieval must be ordered and exclude out-of-root evidence"
    );
    assert_eq!(
        read.optional_text_column(ROOT_DERIVED_CANDIDATE_QUERY, root_derived_params.clone(), 3)
            .await,
        [Some("message-plan-inside".to_string()), None]
    );
    let root_derived_plan = read
        .explain_query_plan(ROOT_DERIVED_CANDIDATE_QUERY, root_derived_params)
        .await;
    assert!(
        root_derived_plan.iter().any(|detail| {
            detail.contains("SESSION_OCCURRENCES_FTS") && detail.contains("VIRTUAL TABLE INDEX")
        }),
        "root derived retrieval must use one live occurrence FTS operator: {root_derived_plan:?}"
    );
    assert!(
        root_derived_plan
            .iter()
            .any(|detail| detail.contains("IDX_SESSION_DERIVED_EVIDENCE_SCOPE_ORDER")),
        "root derived retrieval must use the live evidence scope index: {root_derived_plan:?}"
    );
    let root_derived_wrong_provider = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Text("span".to_string()),
        SqlValue::Text("codex".to_string()),
        SqlValue::Text(fts_phrase("derived needle")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(10),
    ];
    assert!(
        read.text_column(ROOT_DERIVED_CANDIDATE_QUERY, root_derived_wrong_provider, 0)
            .await
            .is_empty(),
        "root derived retrieval must fail closed for the wrong provider"
    );
    let root_derived_missing_phrase = vec![
        SqlValue::Text("user".to_string()),
        SqlValue::Text("span".to_string()),
        SqlValue::Text("claude".to_string()),
        SqlValue::Text(fts_phrase("absent derived phrase")),
        SqlValue::Integer(i64::MAX),
        SqlValue::Text(String::new()),
        SqlValue::Text(String::new()),
        SqlValue::Integer(10),
    ];
    assert!(
        read.text_column(ROOT_DERIVED_CANDIDATE_QUERY, root_derived_missing_phrase, 0)
            .await
            .is_empty(),
        "root derived retrieval must require an FTS-matching member"
    );
}

#[tokio::test]
async fn exact_candidate_matches_embedded_literal_and_returns_utf8_byte_range() {
    let dir = tempdir().expect("temporary directory");
    let conn = TestConnection::open(&dir.path().join("exact-range.db"));
    conn.execute_batch(
        "CREATE TABLE observations (
             observation_id TEXT PRIMARY KEY,
             observation_json TEXT NOT NULL
         );
         CREATE TABLE session_occurrences (
             session_id TEXT NOT NULL,
             generation INTEGER NOT NULL,
             occurrence_id TEXT NOT NULL,
             source_observation_id TEXT NOT NULL,
             retrieval_anchor_id TEXT NOT NULL,
             message_id TEXT,
             turn_id TEXT,
             role TEXT NOT NULL,
             knowledge_at INTEGER NOT NULL,
             snippet_text TEXT NOT NULL,
             PRIMARY KEY(session_id, generation, occurrence_id)
         );
         INSERT INTO observations VALUES (
             'observation-1',
             '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'
         );
         INSERT INTO session_occurrences VALUES
             (
                 'session-snapshot', 1, 'occurrence-exact', 'observation-1',
                 'anchor-exact', 'message-1', 'turn-1', 'user', 2,
                 'prefix 日本語 🚨 middle 日本語 🚨 suffix'
             ),
             (
                 'session-snapshot', 1, 'occurrence-neighbor', 'observation-1',
                 'anchor-neighbor', 'message-2', 'turn-2', 'user', 1,
                 'generic semantic neighbor'
             );",
    )
    .await
    .expect("exact fixture");

    let literal = "日本語 🚨";
    let mut rows = conn
        .query(
            EXACT_CANDIDATE_QUERY,
            vec![
                SqlValue::Text("session-snapshot".to_string()),
                SqlValue::Integer(1),
                SqlValue::Null,
                SqlValue::Text(literal.to_string()),
                SqlValue::Integer(i64::MAX),
                SqlValue::Text(String::new()),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(1_024),
                SqlValue::Integer(
                    i64::try_from(MAX_OBSERVATION_RECORD_BYTES).expect("source byte cap"),
                ),
                SqlValue::Integer(10),
            ],
        )
        .await
        .expect("exact query");
    let row = rows
        .next()
        .await
        .expect("exact row read")
        .expect("embedded exact match");
    let candidate = candidate_from_row(
        &row,
        CandidateChannel::ExactMessage,
        &TemporalRetrievalScope::Session(SessionId::new("session-snapshot").expect("session")),
    )
    .expect("typed exact candidate");

    assert_eq!(candidate.retriever_record_id, "occurrence-exact");
    let first_start = "prefix ".len();
    let second_start = "prefix 日本語 🚨 middle ".len();
    assert_eq!(
        candidate.exact_ranges,
        [
            tracedecay_domain::ByteRangeV1::new(
                u64::try_from(first_start).expect("first start"),
                u64::try_from(first_start + literal.len()).expect("first end"),
            )
            .expect("first exact range"),
            tracedecay_domain::ByteRangeV1::new(
                u64::try_from(second_start).expect("second start"),
                u64::try_from(second_start + literal.len()).expect("second end"),
            )
            .expect("second exact range"),
        ]
    );
    assert!(
        rows.next().await.expect("remaining row read").is_none(),
        "an approximate neighbor cannot be promoted into the exact tier"
    );
}

#[tokio::test]
async fn exact_candidates_do_not_charge_contract_sized_source_against_compact_item_cap() {
    let dir = tempdir().expect("temporary directory");
    let conn = TestConnection::open(&dir.path().join("exact-large-source.db"));
    conn.execute_batch(
        "CREATE TABLE sessions (
             provider TEXT NOT NULL,
             session_id TEXT NOT NULL,
             project_key TEXT NOT NULL,
             PRIMARY KEY(provider, session_id)
         );
         CREATE TABLE retrieval_anchors (
             anchor_id TEXT PRIMARY KEY,
             owner_json TEXT NOT NULL
         );
         CREATE TABLE session_temporal_generations (
             session_id TEXT NOT NULL,
             generation INTEGER NOT NULL,
             state TEXT NOT NULL,
             PRIMARY KEY(session_id, generation)
         );
         CREATE TABLE observations (
             observation_id TEXT PRIMARY KEY,
             observation_json TEXT NOT NULL
         );
         CREATE TABLE session_occurrences (
             session_id TEXT NOT NULL,
             generation INTEGER NOT NULL,
             occurrence_id TEXT NOT NULL,
             source_observation_id TEXT NOT NULL,
             retrieval_anchor_id TEXT NOT NULL,
             message_id TEXT,
             turn_id TEXT,
             role TEXT NOT NULL,
             knowledge_at INTEGER NOT NULL,
             snippet_text TEXT NOT NULL,
             PRIMARY KEY(session_id, generation, occurrence_id)
         );
         INSERT INTO sessions VALUES ('claude', 'session-large', 'user');
         INSERT INTO retrieval_anchors VALUES (
             'anchor-large',
             '{\"kind\":\"profile\"}'
         );
         INSERT INTO session_temporal_generations VALUES (
             'session-large', 1, 'active'
         );
         INSERT INTO observations VALUES (
             'observation-large',
             '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'
         );",
    )
    .await
    .expect("large exact fixture schema");
    let literal = "exact-tail-🚨";
    let prefix_bytes = 768 * 1024;
    let snippet = format!("{}{literal}", "x".repeat(prefix_bytes));
    assert!(snippet.len() < MAX_OBSERVATION_RECORD_BYTES);
    conn.execute(
        "INSERT INTO session_occurrences VALUES (
             'session-large', 1, 'occurrence-large', 'observation-large',
             'anchor-large', 'message-large', 'turn-large', 'user', 1, ?1
         )",
        vec![SqlValue::Text(snippet)],
    )
    .await
    .expect("large exact occurrence");
    let oversized_snippet = format!(
        "{}{literal}",
        "y".repeat(MAX_OBSERVATION_RECORD_BYTES.saturating_add(1))
    );
    conn.execute(
        "INSERT INTO session_occurrences VALUES (
             'session-large', 1, 'occurrence-oversized', 'observation-large',
             'anchor-large', 'message-oversized', 'turn-oversized', 'user', 2, ?1
         )",
        vec![SqlValue::Text(oversized_snippet)],
    )
    .await
    .expect("oversized hostile occurrence");

    let candidate_item_bytes = 256 * 1024;
    let source_bytes = i64::try_from(MAX_OBSERVATION_RECORD_BYTES).expect("source byte cap");
    let mut session_rows = conn
        .query(
            EXACT_CANDIDATE_QUERY,
            vec![
                SqlValue::Text("session-large".to_string()),
                SqlValue::Integer(1),
                SqlValue::Null,
                SqlValue::Text(literal.to_string()),
                SqlValue::Integer(i64::MAX),
                SqlValue::Text(String::new()),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(candidate_item_bytes),
                SqlValue::Integer(source_bytes),
                SqlValue::Integer(1),
            ],
        )
        .await
        .expect("session exact query");
    assert_eq!(
        session_rows
            .next()
            .await
            .expect("session row read")
            .expect("large session exact candidate")
            .get::<String>(0)
            .expect("session occurrence id"),
        "occurrence-large"
    );

    let mut root_rows = conn
        .query(
            ROOT_EXACT_CANDIDATE_QUERY,
            vec![
                SqlValue::Text("user".to_string()),
                SqlValue::Null,
                SqlValue::Text(literal.to_string()),
                SqlValue::Integer(i64::MAX),
                SqlValue::Text(String::new()),
                SqlValue::Text(String::new()),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(candidate_item_bytes),
                SqlValue::Integer(128),
                SqlValue::Integer(source_bytes),
                SqlValue::Integer(1),
            ],
        )
        .await
        .expect("root exact query");
    assert_eq!(
        root_rows
            .next()
            .await
            .expect("root row read")
            .expect("large root exact candidate")
            .get::<String>(0)
            .expect("root occurrence id"),
        "occurrence-large"
    );
}

#[tokio::test]
async fn root_record_authority_binds_the_candidate_source_provider() {
    let dir = tempdir().unwrap();
    let conn = TestConnection::open(&dir.path().join("root-record-authority.db"));
    conn.execute_batch(
        "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL
             );
             INSERT INTO sessions VALUES
                ('provider-good', 'shared-session', 'user'),
                ('provider-bad', 'shared-session', 'different-project');
             INSERT INTO retrieval_anchors VALUES
                ('anchor-1', '{\"kind\":\"profile\"}');
             INSERT INTO session_temporal_generations VALUES
                ('shared-session', 1, 'active');
             INSERT INTO observations VALUES (
                'observation-bad',
                '{\"identity\":{\"source\":{\"provider\":\"provider-bad\"}}}'
             );
             INSERT INTO session_occurrences VALUES (
                'shared-session', 1, 'occurrence-1', 'observation-bad', 'anchor-1'
             );",
    )
    .await
    .unwrap();
    let mut candidate = candidate_for_anchor("anchor-1");
    candidate.session = Some("shared-session".to_string());
    candidate.source = Some("occurrence-1".to_string());
    assert!(
        require_candidate_root_authority(
            &super::super::sql::TemporalSqlRead::engine_connection(&conn),
            &candidate,
            "user",
            None,
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn profile_root_authorizes_legacy_claude_anchor_without_project_scope() {
    let dir = tempdir().unwrap();
    let conn = TestConnection::open(&dir.path().join("profile-root-authority.db"));
    conn.execute_batch(
        "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL
             );
             INSERT INTO sessions VALUES
                ('claude', 'profile-session', 'user');
             INSERT INTO retrieval_anchors VALUES
                ('profile-anchor', '{\"kind\":\"profile\"}');
             INSERT INTO session_temporal_generations VALUES
                ('profile-session', 1, 'active');
             INSERT INTO observations VALUES (
                'profile-observation',
                '{\"identity\":{\"source\":{}}}'
             );
             INSERT INTO session_occurrences VALUES (
                'profile-session', 1, 'profile-occurrence',
                'profile-observation', 'profile-anchor'
             );",
    )
    .await
    .unwrap();
    let mut candidate = candidate_for_anchor("profile-anchor");
    candidate.session = Some("profile-session".to_string());
    candidate.source = Some("profile-occurrence".to_string());
    candidate.retriever_record_id = "profile-occurrence".to_string();
    let read = super::super::sql::TemporalSqlRead::engine_connection(&conn);

    require_candidate_root_authority(&read, &candidate, "user", Some("claude"))
        .await
        .expect("profile root must authorize the legacy Claude provider fallback");
    assert!(
        require_candidate_root_authority(&read, &candidate, "associated-project", Some("claude"),)
            .await
            .is_err(),
        "a profile anchor must not become project-owned through session metadata"
    );
}

#[tokio::test]
async fn provider_filter_separates_same_session_and_none_reads_all_providers() {
    let dir = tempdir().unwrap();
    let conn = TestConnection::open(&dir.path().join("provider-scope.db"));
    conn.execute_batch(
        "CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );
             INSERT INTO observations VALUES
                ('observation-claude', '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'),
                ('observation-codex', '{\"identity\":{\"source\":{\"provider\":\"codex\"}}}');
             INSERT INTO session_occurrences VALUES
                ('shared-session', 1, 'occurrence-claude', 'observation-claude',
                 'anchor-claude', 'message-claude', NULL, 'user', 2),
                ('shared-session', 1, 'occurrence-codex', 'observation-codex',
                 'anchor-codex', 'message-codex', NULL, 'user', 1);",
    )
    .await
    .unwrap();

    async fn occurrence_ids(
        conn: &Connection,
        provider: SqlValue,
    ) -> tracedecay_runtime_core::db::engine::Result<Vec<String>> {
        let mut rows = conn
            .query(
                TIME_CANDIDATE_QUERY,
                vec![
                    SqlValue::Text("shared-session".to_string()),
                    SqlValue::Integer(1),
                    provider,
                    SqlValue::Integer(0),
                    SqlValue::Integer(10),
                    SqlValue::Integer(i64::MAX),
                    SqlValue::Text(String::new()),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(1024),
                    SqlValue::Integer(10),
                ],
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get(0)?);
        }
        Ok(ids)
    }

    assert_eq!(
        occurrence_ids(&conn, SqlValue::Text("claude".to_string()))
            .await
            .unwrap(),
        ["occurrence-claude"]
    );
    assert_eq!(
        occurrence_ids(&conn, SqlValue::Null).await.unwrap(),
        ["occurrence-claude", "occurrence-codex"]
    );
}

#[tokio::test]
async fn root_pagination_restart_provider_filter_and_session_parity_are_stable() {
    let dir = tempdir().unwrap();
    let conn = TestConnection::open(&dir.path().join("root-pagination.db"));
    conn.execute_batch(
        "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );
             CREATE INDEX idx_session_occurrences_root_generation_order
                ON session_occurrences(
                    knowledge_at DESC, session_id, occurrence_id, generation
                );
             INSERT INTO session_temporal_generations VALUES
                ('session-a', 1, 'active'),
                ('session-b', 1, 'active'),
                ('session-c', 1, 'active');
             INSERT INTO observations VALUES
                ('observation-claude', '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'),
                ('observation-codex', '{\"identity\":{\"source\":{\"provider\":\"codex\"}}}');
             INSERT INTO retrieval_anchors VALUES
                ('same-anchor', '{\"kind\":\"profile\"}');
             INSERT INTO sessions VALUES
                ('claude', 'session-a', 'user'),
                ('claude', 'session-b', 'user'),
                ('codex', 'session-c', 'user');
             INSERT INTO session_occurrences VALUES
                ('session-a', 1, 'same-id', 'observation-claude',
                 'same-anchor', 'same-message', NULL, 'user', 5),
                ('session-b', 1, 'same-id', 'observation-claude',
                 'same-anchor', 'same-message', NULL, 'user', 5),
                ('session-c', 1, 'same-id', 'observation-codex',
                 'same-anchor', 'same-message', NULL, 'user', 5);",
    )
    .await
    .unwrap();

    async fn root_rows(
        conn: &Connection,
        provider: SqlValue,
        cursor: (i64, &str, &str),
        limit: i64,
    ) -> Vec<(String, String)> {
        let mut rows = conn
            .query(
                ROOT_TIME_CANDIDATE_QUERY,
                vec![
                    SqlValue::Text("user".to_string()),
                    provider,
                    SqlValue::Integer(0),
                    SqlValue::Integer(10),
                    SqlValue::Integer(cursor.0),
                    SqlValue::Text(cursor.1.to_string()),
                    SqlValue::Text(cursor.2.to_string()),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(limit),
                ],
            )
            .await
            .unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            values.push((row.get(5).unwrap(), row.get(0).unwrap()));
        }
        values
    }

    let first = root_rows(&conn, SqlValue::Null, (i64::MAX, "", ""), 1).await;
    assert_eq!(first, [("session-a".to_string(), "same-id".to_string())]);
    let continuation = (5, first[0].0.as_str(), first[0].1.as_str());
    let second = root_rows(&conn, SqlValue::Null, continuation, 1).await;
    let restarted = root_rows(&conn, SqlValue::Null, continuation, 1).await;
    assert_eq!(second, [("session-b".to_string(), "same-id".to_string())]);
    assert_eq!(restarted, second);
    assert_eq!(
        root_rows(
            &conn,
            SqlValue::Text("claude".to_string()),
            (i64::MAX, "", ""),
            10,
        )
        .await,
        [
            ("session-a".to_string(), "same-id".to_string()),
            ("session-b".to_string(), "same-id".to_string()),
        ]
    );

    conn.execute(
        "UPDATE session_temporal_generations
             SET state = 'superseded'
             WHERE session_id <> 'session-a'",
        (),
    )
    .await
    .unwrap();
    let root = root_rows(&conn, SqlValue::Null, (i64::MAX, "", ""), 10).await;
    let mut session_rows = conn
        .query(
            TIME_CANDIDATE_QUERY,
            vec![
                SqlValue::Text("session-a".to_string()),
                SqlValue::Integer(1),
                SqlValue::Null,
                SqlValue::Integer(0),
                SqlValue::Integer(10),
                SqlValue::Integer(i64::MAX),
                SqlValue::Text(String::new()),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(1_024),
                SqlValue::Integer(10),
            ],
        )
        .await
        .unwrap();
    let mut session = Vec::new();
    while let Some(row) = session_rows.next().await.unwrap() {
        session.push((row.get(5).unwrap(), row.get(0).unwrap()));
    }
    assert_eq!(
        root, session,
        "single-session root scope must preserve session semantics"
    );
}

#[tokio::test]
async fn root_record_hydration_rejects_cross_session_copy_and_assertion_traps() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_cross_session_record_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    let snapshot = root_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let mut candidate_a = candidate_for_anchor("same-anchor");
    candidate_a.session = Some("session-a".to_string());
    let kinds_a = read
        .record_kinds(&snapshot, candidate_a, &record_request())
        .await;
    assert_eq!(kinds_a, ["occurrence"]);

    let mut candidate_b = candidate_for_anchor("same-anchor");
    candidate_b.session = Some("session-b".to_string());
    let kinds_b = read
        .record_kinds(&snapshot, candidate_b, &record_request())
        .await;
    assert!(kinds_b.contains(&"occurrence".to_string()));
    assert!(kinds_b.contains(&"assertion".to_string()));
    assert!(kinds_b.contains(&"copy".to_string()));
}

#[tokio::test]
async fn current_record_hydration_retains_non_superseding_assertions_for_resolution() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_cross_session_record_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    let snapshot = root_snapshot_with_mode(1, None, TemporalModeV1::Current);
    let mut candidate = candidate_for_anchor("same-anchor");
    candidate.session = Some("session-b".to_string());

    let kinds = read
        .record_kinds(&snapshot, candidate, &record_request())
        .await;

    assert!(kinds.contains(&"occurrence".to_string()));
    assert!(
        kinds.contains(&"assertion".to_string()),
        "Current must pass conflict/support assertions to the shared resolver"
    );
}

#[tokio::test]
async fn as_of_summary_source_uses_frozen_horizon_not_later_current_occurrence() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime
        .seed_historical_summary_successor_fixture_for_test()
        .await;
    let read = runtime.retrieval_read_for_test().await;
    let snapshot = scoped_snapshot_with_mode(
        1,
        None,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(6),
        },
    );

    let records = read
        .records(
            &snapshot,
            candidate_for_anchor("historical-summary-anchor"),
            &record_request(),
        )
        .await;
    let source = records
        .iter()
        .find_map(|record| match record {
            TemporalRecord::SummarySource(source) => Some(source),
            _ => None,
        })
        .expect("historical summary source");

    assert_eq!(
        source.state,
        SummarySourceState::Covered {
            knowledge_at: UtcMicros(5),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(5),
            },
        }
    );
}

#[tokio::test]
async fn derived_candidate_materializes_members_with_canonical_evidence_linkage() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_derived_record_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let mut candidate = candidate_for_anchor("derived-span-anchor");
    candidate.channel = CandidateChannel::Span;
    candidate.retriever_record_id = "span-evidence-id".to_string();

    let records = read.records(&snapshot, candidate, &record_request()).await;
    assert_eq!(records.len(), 1);
    let TemporalRecord::Occurrence(member) = &records[0] else {
        panic!("derived candidate must materialize its member occurrence");
    };
    assert_eq!(
        member.anchor_id,
        RetrievalAnchorId::new("source-occurrence-anchor").unwrap()
    );
    assert!(
        member
            .evidence
            .supporting_anchor_ids
            .contains(&RetrievalAnchorId::new("source-evidence-anchor").unwrap())
    );
    assert!(
        member
            .evidence
            .supporting_anchor_ids
            .contains(&RetrievalAnchorId::new("derived-span-anchor").unwrap())
    );
}

#[tokio::test]
async fn oversized_evidence_publication_and_source_json_never_reach_record_rows() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_oversized_record_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;

    let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let request = PageRequest::for_test(32, 4096, 128, 32, 512);
    assert!(
        !read
            .record_kinds(&snapshot, candidate_for_anchor("anchor-evidence"), &request,)
            .await
            .contains(&"occurrence".to_string())
    );
    for anchor in ["anchor-publication", "anchor-source"] {
        assert!(
            !read
                .record_kinds(&snapshot, candidate_for_anchor(anchor), &request)
                .await
                .contains(&"summary".to_string()),
            "oversized summary JSON for {anchor} must be rejected in its UNION arm"
        );
    }
}

#[tokio::test]
async fn summary_source_count_cap_rejects_before_group_array() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_summary_source_cap_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;

    let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let request = PageRequest::for_test(32, 2 * 1024 * 1024, 1024 * 1024, 32, 512);
    let kinds = read
        .record_kinds(
            &snapshot,
            candidate_for_anchor("anchor-many-sources"),
            &request,
        )
        .await;
    assert!(
        !kinds.contains(&"summary".to_string()),
        "257 sources must not be truncated into a 256-source summary JSON array"
    );
}

#[tokio::test]
async fn provider_specific_summary_requires_retained_provider_evidence() {
    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_provider_summary_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    let request = record_request();
    let candidate = || candidate_for_anchor("anchor-summary-provider");

    let claude = read
        .record_kinds(&scoped_snapshot(1, Some("claude")), candidate(), &request)
        .await;
    assert!(claude.contains(&"summary".to_string()));

    let codex = read
        .record_kinds(&scoped_snapshot(1, Some("codex")), candidate(), &request)
        .await;
    assert!(!codex.contains(&"summary".to_string()));

    let all = read
        .record_kinds(&scoped_snapshot(1, None), candidate(), &request)
        .await;
    assert!(all.contains(&"summary".to_string()));
}

#[tokio::test]
async fn record_query_plan_is_keyset_indexed_without_per_candidate_work() {
    let total = 100_000usize;
    let start = 71_111usize;
    let end = bounded_window_end(total, start, 38);
    assert_eq!(end - start, 38);

    let dir = tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_candidate_query_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    let candidates = [
        (
            "occurrence-plan-inside",
            "anchor-plan-inside",
            "message-plan-inside",
            20,
        ),
        (
            "occurrence-plan-inside-old",
            "anchor-plan-inside-old",
            "message-plan-inside-old",
            10,
        ),
    ]
    .into_iter()
    .map(
        |(occurrence_id, anchor_id, message_id, knowledge_at)| RankingCandidate {
            stable_id: format!("exact:{occurrence_id}"),
            anchor_id: RetrievalAnchorId::new(anchor_id).expect("anchor"),
            retriever_record_id: occurrence_id.to_string(),
            channel: CandidateChannel::ExactMessage,
            raw_score: 1_000,
            knowledge_at_micros: knowledge_at,
            logical_message: Some(message_id.to_string()),
            turn: None,
            session: Some("session-plan-inside".to_string()),
            source: Some("claude".to_string()),
            evidence_role: Some("user".to_string()),
            exact_ranges: Vec::new(),
        },
    )
    .collect::<Vec<_>>();
    let request = PageRequest::for_test(37, 64 * 1024, 8 * 1024, 37, 512);
    let snapshot = scoped_snapshot_with_mode(1, Some("claude"), TemporalModeV1::Forensic);
    let query = build_record_query(
        &TemporalRetrievalScope::Session(SessionId::new("session-plan-inside").expect("session")),
        &snapshot,
        &candidates,
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        38,
        &request,
    )
    .expect("bounded record query");
    assert_eq!(
        read.text_column(&query.sql, query.params.clone(), 2).await,
        ["occurrence-plan-inside", "occurrence-plan-inside-old"],
        "the populated record query must preserve candidate keyset order"
    );
    let plan = read.explain_record_query(query).await;
    assert!(
        plan.iter()
            .any(|detail| detail.contains("IDX_SESSION_OCCURRENCES_ANCHOR_ORDER")),
        "record hydration must use the live occurrence-anchor index: {plan:?}"
    );
    assert!(
        plan.iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE")),
        "record hydration must not materialize any temporary B-tree sort: {plan:?}"
    );
    assert!(
        plan.iter().all(|detail| !detail.contains("CORRELATED")),
        "record hydration must not execute correlated per-candidate work: {plan:?}"
    );

    let page_after_first = build_record_query(
        snapshot.retrieval_scope(),
        &snapshot,
        &candidates,
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: "session-plan-inside".to_string(),
            stable_id: "occurrence-plan-inside".to_string(),
        },
        38,
        &request,
    )
    .expect("record keyset query");
    assert_eq!(
        read.text_column(&page_after_first.sql, page_after_first.params, 2)
            .await,
        ["occurrence-plan-inside-old"],
        "the record cursor must resume strictly after the first populated row"
    );
}

#[test]
fn fts_values_are_bound_as_literal_phrases() {
    assert_eq!(fts_phrase("hello world"), "\"hello world\"");
    assert_eq!(fts_phrase("say \"hello\""), "\"say \"\"hello\"\"\"");
}

#[test]
fn iso_day_bounds_are_micros_and_half_open() {
    let (start, end) = iso_day_bounds("2026-07-18").unwrap();
    assert_eq!(end - start, 86_400_000_000);
    assert!(iso_day_bounds("not-a-date").is_err());
}
