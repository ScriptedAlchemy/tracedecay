use std::{fs, path::PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_sqlite_parity_protocol::{
    Command, CopiedDatabase, CopiedSnapshotProvenance, DatabaseKind, Output, PROTOCOL_VERSION,
    Request, ResponseOutcome, SnapshotFileIdentity,
};

use crate::{service::handle_request_bytes, snapshot::sealed_file_metadata};

pub(super) struct Fixture {
    pub(super) _directory: TempDir,
    pub(super) path: PathBuf,
}

pub(super) fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("copied-東京.db");
    let connection = Connection::open(&path).expect("create fixture");
    connection
        .execute_batch(
            "
            PRAGMA page_size = 4096;
            PRAGMA journal_mode = DELETE;
            PRAGMA user_version = 7;
            CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE sanitization_receipts (
                receipt_id TEXT PRIMARY KEY,
                sanitizer_version TEXT NOT NULL,
                payload_digest TEXT NOT NULL,
                receipt_json TEXT NOT NULL
            );
            CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                payload_digest TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                observation_json TEXT NOT NULL,
                committed_cursor_json TEXT NOT NULL,
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
            );",
        )
        .expect("create schema");
    connection
        .execute_batch(crate::fixture_ddl::SESSION_STORE_FIXTURE_TABLES_DDL)
        .expect("create shared session-store schema");
    connection
        .execute_batch(
            "INSERT INTO sanitization_receipts VALUES ('receipt', 'v1', 'payloads', '{}');
             INSERT INTO observations(
                 observation_id, payload_digest, receipt_id, observation_json,
                 committed_cursor_json
             ) VALUES
                 ('observation-1', 'digest-1', 'receipt', '{}', '{}'),
                 ('observation-2', 'digest-2', 'receipt', '{}', '{}');
             INSERT INTO source_cursors(source_json, scope_json, cursor_json) VALUES
                 ('{\"source\":\"a\"}', '{\"scope\":\"1\"}', '{\"cursor\":\"1\"}'),
                 ('{\"source\":\"a\"}', '{\"scope\":\"2\"}', '{\"cursor\":\"2\"}');
             INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('codex', 'session-1', 'project', '/copy');
             INSERT INTO session_messages(
                 provider, message_id, session_id, role, ordinal, text
             ) VALUES
                 ('codex', 'message-1', 'session-1', 'user', 0, 'one'),
                 ('codex', 'message-2', 'session-1', 'assistant', 1, 'two');
             INSERT INTO session_schema_migrations VALUES ('hermes-lcm', 11, 1);
             INSERT INTO lcm_raw_messages(
                 provider, message_id, session_id, role, ordinal, content, content_hash,
                 storage_kind, snippet_text, index_text
             ) VALUES
                 ('codex', 'message-1', 'session-1', 'user', 0, 'one', 'hash-1',
                  'inline', 'one', 'one'),
                 ('codex', 'message-2', 'session-1', 'assistant', 1, 'two', 'hash-2',
                  'inline', 'two', 'two');
             INSERT INTO session_temporal_schema_migrations
             VALUES ('session-temporal', 3, 1);
             INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at
             ) VALUES ('session-1', 1, 'ready', '{}', 1);
             INSERT INTO session_temporal_observation_effects(
                 observation_id, observation_sequence, session_id, receipt_id,
                 effect_digest, output_count, recorded_at
             ) VALUES ('observation-1', 1, 'session-1', 'receipt', 'effect-1', 1, 1);
             INSERT INTO session_temporal_projection_receipts(
                 session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
                 source_through, projection_through, occurrence_count, occurrence_digest,
                 dimension_count, dimension_digest, copy_count, copy_digest, assertion_count,
                 assertion_digest, supersession_count, supersession_digest, current_count,
                 current_digest, fts_count, fts_digest, committed_at
             ) VALUES
                 ('session-1', 1, 0, 'batch-0', '{}', 0, 0, 1, 'occ', 0, 'dim', 0, 'copy',
                  0, 'assert', 0, 'super', 0, 'curr', 0, 'fts', 1),
                 ('session-1', 1, 1, 'batch-1', '{}', 1, 1, 0, 'occ', 0, 'dim', 0, 'copy',
                  0, 'assert', 0, 'super', 0, 'curr', 0, 'fts', 2);
             INSERT INTO session_occurrences(
                 session_id, generation, occurrence_id, source_observation_id,
                 projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                 valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES
                 ('session-1', 1, 'occurrence-1', 'observation-1', 0, 'anchor-1', 'user', 1,
                  '{\"kind\":\"unknown\"}', '{}', 'snippet', 'index'),
                 ('session-1', 1, 'occurrence-2', 'observation-1', 1, 'anchor-2', 'assistant', 2,
                  '{\"kind\":\"unknown\"}', '{}', 'snippet', 'index');
             INSERT INTO session_logical_copy_edges(
                 session_id, generation, occurrence_id, copied_from_occurrence_id,
                 proof_json, knowledge_at, valid_time_json, created_at
             ) VALUES
                 ('session-1', 1, 'occurrence-2', 'occurrence-1', '{}', 2,
                  '{\"kind\":\"unknown\"}', 2),
                 ('session-1', 1, 'occurrence-3', 'occurrence-1', '{}', 3,
                  '{\"kind\":\"unknown\"}', 3);
             INSERT INTO session_assertions(
                 session_id, generation, assertion_id, assertion_kind, subject_anchor_id,
                 object_anchor_id, knowledge_at, valid_time_json, evidence_json
             ) VALUES
                 ('session-1', 1, 'assertion-1', 'supersedes', 'anchor-1', 'anchor-2', 1,
                  '{\"kind\":\"unknown\"}', '{}'),
                 ('session-1', 1, 'assertion-2', 'annotates', 'anchor-2', 'anchor-1', 2,
                  '{\"kind\":\"unknown\"}', '{}');
             INSERT INTO session_summary_nodes(
                 summary_id, session_id, summary_anchor_id, summary_text, index_text,
                 source_horizon_json, created_at
             ) VALUES
                 ('summary-1', 'session-1', 'anchor-1', '', '', '{}', 1),
                 ('summary-2', 'session-1', 'anchor-2', '', '', '{}', 2);
             INSERT INTO session_summary_sources(
                 summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES
                 ('summary-1', 0, 'anchor', 'anchor-1', NULL),
                 ('summary-1', 1, 'summary', NULL, 'summary-1');
             INSERT INTO session_summary_successors(
                 predecessor_summary_id, successor_summary_id, created_at
             ) VALUES
                 ('summary-1', 'summary-2', 1),
                 ('summary-1', 'summary-3', 2);
             INSERT INTO memory_v2_facts(
                 fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES
                 ('fact-1', 'project', 'proj', '{}', '{}', 1),
                 ('fact-2', 'project', 'proj', '{}', '{}', 2);
             INSERT INTO memory_v2_assertions(
                 assertion_id, fact_id, owner_kind, project_id, owner_json,
                 assertion_header_json, kind_json, payload_reference_json, receipt_json,
                 asserted_at, actor_id
             ) VALUES
                 ('assertion-1', 'fact-1', 'project', 'proj', '{}', '{}', '{}', '{}', '{}', 1,
                  NULL),
                 ('assertion-2', 'fact-1', 'project', 'proj', '{}', '{}', '{}', '{}', '{}', 2,
                  NULL);
             INSERT INTO memory_v2_lineage_events(
                 event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
             ) VALUES
                 ('event-1', 'fact-1', 'project', 'proj', '{}', 1, 1),
                 ('event-2', 'fact-1', 'project', 'proj', '{}', 2, 2);
             INSERT INTO memory_v2_current_facts(
                 fact_id, owner_kind, project_id, payload_access, last_event_id, updated_at,
                 projection_state
             ) VALUES
                 ('fact-1', 'project', 'proj', 'eligible', 'event-1', 1, 'ready'),
                 ('fact-2', 'project', 'proj', 'redacted', 'event-2', 2, 'stale');
             INSERT INTO retrieval_anchors(
                 anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                 ('anchor-1', '{}', '{}', 'generation-1'),
                 ('anchor-2', '{}', '{}', 'generation-2');
             INSERT INTO generation_diagnostics(
                 diagnostic_anchor, generation_id, repository, file_occurrence_id,
                 content_digest, span_start, span_end, code, severity, message,
                 message_digest, producer_kind, producer, analyzer_revision,
                 configuration_revision, evidence_class, collected_at, record_state,
                 persisted_at
             ) VALUES
                 ('diagnostic-1', 'generation-1', 'repo', 'file-1', 'content-1', 0, 4,
                  'E0001', 'error', 'boom', 'message-1', 'compiler', 'rustc', 'r1', 'c1',
                  'observed', 1, 'current', 1),
                 ('diagnostic-2', 'generation-1', 'repo', 'file-1', 'content-1', 5, 9,
                  'W0001', 'warning', 'hmm', 'message-2', 'compiler', 'rustc', 'r1', 'c1',
                  'observed', 2, 'superseded', 2);
             INSERT INTO diagnostic_generation_publications(
                 generation_id, record_state, state_generation, published_at
             ) VALUES
                 ('generation-1', 'superseded', 'generation-2', 1),
                 ('generation-2', 'current', NULL, 2);
             INSERT INTO configuration_revisions(
                 revision_id, parent_revision_id, snapshot_id, effective_behavior_digest,
                 resolution_provenance_digest, actor_id, operation_kind, created_at
             ) VALUES
                 ('revision-1', NULL, 'snapshot-1', 'behavior-1', 'provenance-1', 'actor',
                  'bootstrap', 1),
                 ('revision-2', 'revision-1', 'snapshot-2', 'behavior-2', 'provenance-2',
                  'actor', 'mutate', 2);
             INSERT INTO configuration_entries(
                 revision_id, key, layer_kind, layer_id, schema_revision, typed_value
             ) VALUES
                 ('revision-1', 'key-1', 'layer', 'layer-1', 1, 'value-1'),
                 ('revision-1', 'key-1', 'layer', 'layer-2', 1, 'value-2');
             INSERT INTO configuration_mutation_receipts(
                 receipt_id, plan_id, actor_id, idempotency_key, base_revision_id,
                 result_revision_id, operation_digest, authorization_policy_digest,
                 activation_status, receipt_digest, created_at
             ) VALUES
                 ('receipt-1', NULL, 'actor', 'idempotency-1', 'revision-1', 'revision-2',
                  'operation-1', 'policy-1', 'activated', 'receipt-digest-1', 2),
                 ('receipt-2', NULL, 'actor', 'idempotency-2', 'revision-2', 'revision-2',
                  'operation-2', 'policy-1', 'noop', 'receipt-digest-2', 3);
             INSERT INTO configuration_audit_events(
                 event_id, actor_id, idempotency_key, operation_kind, base_revision_id,
                 result_revision_id, sealed_target_reference,
                 event_scoped_target_commitment, receipt_digest, correlation_id,
                 safe_reason_code, occurred_at
             ) VALUES
                 ('event-1', 'actor', 'idempotency-1', 'mutate', 'revision-1', 'revision-2',
                  NULL, 'commitment-1', 'receipt-digest-1', NULL, NULL, 2),
                 ('event-2', 'actor', NULL, 'denied', 'revision-2', NULL, NULL,
                  'commitment-2', NULL, NULL, 'unauthorized', 3);",
        )
        .expect("insert session-store fixture rows");
    drop(connection);
    Fixture {
        _directory: directory,
        path,
    }
}

pub(super) fn copied_database(path: &std::path::Path) -> CopiedDatabase {
    let canonical_path = fs::canonicalize(path).expect("canonicalize copied fixture");
    let (byte_len, content_digest, file_identity) =
        sealed_file_metadata(&canonical_path).expect("seal copied fixture");
    CopiedDatabase {
        path: canonical_path.clone(),
        kind: DatabaseKind::CopiedSnapshot,
        provenance: CopiedSnapshotProvenance {
            authority_identity: "test:copied-snapshot".to_owned(),
            staging_root: canonical_path
                .parent()
                .expect("copied fixture parent")
                .to_path_buf(),
            canonical_path,
            byte_len,
            content_digest,
            file_identity,
        },
    }
}

pub(super) fn missing_copied_database(path: &std::path::Path) -> CopiedDatabase {
    CopiedDatabase {
        path: path.to_path_buf(),
        kind: DatabaseKind::CopiedSnapshot,
        provenance: CopiedSnapshotProvenance {
            authority_identity: "test:missing-snapshot".to_owned(),
            staging_root: path.parent().expect("missing fixture parent").to_path_buf(),
            canonical_path: path.to_path_buf(),
            byte_len: 0,
            content_digest: format!("sha256:{}", "0".repeat(64)),
            file_identity: SnapshotFileIdentity::Unsupported,
        },
    }
}

pub(super) fn request_value(
    path: &std::path::Path,
    request_id: &str,
    command: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request_id,
        "database": copied_database(path),
        "command": command,
    })
}

pub(super) fn execute(path: &std::path::Path, command: Command) -> Output {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "unit".to_string(),
        database: copied_database(path),
        command,
    };
    let bytes = serde_json::to_vec(&request).expect("serialize request");
    match handle_request_bytes(&bytes).outcome {
        ResponseOutcome::Ok { output } => output,
        ResponseOutcome::Error { error } => panic!("unexpected error: {error:?}"),
    }
}
