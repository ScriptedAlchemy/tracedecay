use std::{fs, path::PathBuf};

use rusqlite::{Connection, params};
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
            CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                docstring TEXT,
                signature TEXT
            );
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
            );
            CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                title TEXT,
                started_at INTEGER,
                ended_at INTEGER,
                transcript_path TEXT,
                metadata_json TEXT,
                parent_session_id TEXT,
                is_subagent INTEGER NOT NULL DEFAULT 0,
                agent_id TEXT,
                parent_tool_use_id TEXT,
                PRIMARY KEY(provider, session_id)
            );
            CREATE TABLE session_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                timestamp INTEGER,
                ordinal INTEGER NOT NULL,
                text TEXT NOT NULL,
                kind TEXT,
                model TEXT,
                tool_names TEXT,
                source_path TEXT,
                source_offset INTEGER,
                metadata_json TEXT,
                PRIMARY KEY(provider, message_id),
                FOREIGN KEY(provider, session_id)
                    REFERENCES sessions(provider, session_id) ON DELETE CASCADE
            );
            CREATE TABLE session_schema_migrations (
                name TEXT PRIMARY KEY,
                version INTEGER NOT NULL,
                applied_at INTEGER NOT NULL
            );
            CREATE TABLE lcm_raw_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                timestamp INTEGER,
                content TEXT,
                content_hash TEXT NOT NULL,
                storage_kind TEXT NOT NULL,
                payload_ref TEXT,
                snippet_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                legacy_source INTEGER NOT NULL DEFAULT 0,
                legacy_truncated INTEGER NOT NULL DEFAULT 0,
                metadata_json TEXT,
                UNIQUE(provider, message_id),
                FOREIGN KEY(provider, session_id)
                    REFERENCES sessions(provider, session_id) ON DELETE CASCADE
            );
            CREATE TABLE session_temporal_schema_migrations (
                name TEXT PRIMARY KEY,
                version INTEGER NOT NULL,
                applied_at INTEGER NOT NULL
            );
            CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                frozen_watermarks_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                ready_at INTEGER,
                activated_at INTEGER,
                completed_at INTEGER,
                PRIMARY KEY(session_id, generation)
            );
            CREATE TABLE session_temporal_observation_effects (
                observation_id TEXT PRIMARY KEY,
                observation_sequence INTEGER NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                effect_digest TEXT NOT NULL,
                output_count INTEGER NOT NULL,
                recorded_at INTEGER NOT NULL
            );
            CREATE TABLE session_temporal_projection_receipts (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                batch_ordinal INTEGER NOT NULL,
                batch_digest TEXT NOT NULL,
                frozen_watermarks_json TEXT NOT NULL,
                source_through INTEGER NOT NULL,
                projection_through INTEGER NOT NULL,
                occurrence_count INTEGER NOT NULL,
                occurrence_digest TEXT NOT NULL,
                dimension_count INTEGER NOT NULL,
                dimension_digest TEXT NOT NULL,
                copy_count INTEGER NOT NULL,
                copy_digest TEXT NOT NULL,
                assertion_count INTEGER NOT NULL,
                assertion_digest TEXT NOT NULL,
                supersession_count INTEGER NOT NULL,
                supersession_digest TEXT NOT NULL,
                current_count INTEGER NOT NULL,
                current_digest TEXT NOT NULL,
                fts_count INTEGER NOT NULL,
                fts_digest TEXT NOT NULL,
                committed_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, batch_ordinal),
                UNIQUE(session_id, generation, batch_digest)
            );
            CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                projection_output_ordinal INTEGER NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                thread_id TEXT,
                thread_grouping_json TEXT,
                turn_id TEXT,
                turn_grouping_json TEXT,
                message_id TEXT,
                agent_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                valid_time_json TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                snippet_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
            );
            CREATE TABLE session_summary_nodes (
                summary_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                summary_anchor_id TEXT NOT NULL,
                summary_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                source_horizon_json TEXT NOT NULL,
                publication_json TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE VIRTUAL TABLE nodes_fts USING fts5(
                name, qualified_name, docstring, signature,
                content='nodes', content_rowid='rowid'
            );",
        )
        .expect("create schema");
    connection
        .execute(
            "INSERT INTO nodes(id, name, qualified_name, docstring, signature)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                "node-unicode",
                "東京 café 🦀",
                "crate::東京",
                "Unicode graph node",
                "fn 東京()"
            ],
        )
        .expect("insert Unicode node");
    connection
        .execute_batch(
            "INSERT INTO sanitization_receipts VALUES ('receipt', 'v1', 'payloads', '{}');
             INSERT INTO observations(
                 observation_id, payload_digest, receipt_id, observation_json,
                 committed_cursor_json
             ) VALUES
                 ('observation-1', 'digest-1', 'receipt', '{}', '{}'),
                 ('observation-2', 'digest-2', 'receipt', '{}', '{}');
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
             ) VALUES (
                 'session-1', 1, 'occurrence-1', 'observation-1', 0, 'anchor-1', 'user', 1,
                 '{\"kind\":\"unknown\"}', '{}', 'snippet', 'index'
             );
             INSERT INTO session_summary_nodes(
                 summary_id, session_id, summary_anchor_id, summary_text, index_text,
                 source_horizon_json, created_at
             ) VALUES ('summary-1', 'session-1', 'anchor-1', '', '', '{}', 1);",
        )
        .expect("insert session-store fixture rows");
    connection
        .execute("INSERT INTO nodes_fts(nodes_fts) VALUES ('rebuild')", [])
        .expect("build FTS index");
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
