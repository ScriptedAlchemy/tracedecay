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
             ) VALUES ('observation-1', 1, 'session-1', 'receipt', 'effect-1', 1, 1);",
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
