use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_sqlite_parity_protocol::{
    CopiedDatabase, CopiedSnapshotProvenance, DatabaseKind, SnapshotFileIdentity,
};

const PROTOCOL_VERSION: u16 = 1;

pub(crate) struct Fixture {
    pub(crate) _directory: TempDir,
    pub(crate) path: PathBuf,
}

pub(crate) fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("copied.db");
    let connection = Connection::open(&path).expect("create fixture");
    connection
        .execute_batch(
            "
            PRAGMA user_version = 11;
            CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                docstring TEXT,
                signature TEXT
            );
            CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                payload_digest TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                observation_json TEXT NOT NULL,
                committed_cursor_json TEXT NOT NULL
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
            );
            INSERT INTO nodes VALUES (
                'unicode', 'naïve 東京', 'crate::東京', 'Unicode', 'fn 東京()'
            );
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
            ) VALUES ('codex', 'message-1', 'session-1', 'user', 0, 'hello');
            INSERT INTO session_schema_migrations VALUES ('hermes-lcm', 11, 1);
            INSERT INTO lcm_raw_messages(
                provider, message_id, session_id, role, ordinal, content, content_hash,
                storage_kind, snippet_text, index_text
            ) VALUES (
                'codex', 'message-1', 'session-1', 'user', 0, 'hello', 'hash-1',
                'inline', 'hello', 'hello'
            );
            INSERT INTO session_temporal_schema_migrations
            VALUES ('session-temporal', 3, 1);
            INSERT INTO session_temporal_generations(
                session_id, generation, state, frozen_watermarks_json, created_at
            ) VALUES ('session-1', 1, 'ready', '{}', 1);
            INSERT INTO session_temporal_observation_effects(
                observation_id, observation_sequence, session_id, receipt_id,
                effect_digest, output_count, recorded_at
            ) VALUES ('observation-1', 1, 'session-1', 'receipt', 'effect-1', 1, 1);
            INSERT INTO nodes_fts(nodes_fts) VALUES ('rebuild');",
        )
        .expect("create fixture schema");
    drop(connection);
    Fixture {
        _directory: directory,
        path,
    }
}

pub(crate) fn invoke(request: &Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tracedecay-rusqlite-parity-probe"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn parity helper");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(&serde_json::to_vec(request).expect("serialize request"))
        .expect("write request");
    let output = child.wait_with_output().expect("wait for parity helper");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("versioned JSON response")
}

pub(crate) fn copied_database(path: &Path) -> CopiedDatabase {
    let canonical_path = path.canonicalize().expect("canonicalize copied fixture");
    let metadata = fs::metadata(&canonical_path).expect("read copied fixture metadata");
    let bytes = fs::read(&canonical_path).expect("hash copied fixture");
    CopiedDatabase {
        path: canonical_path.clone(),
        kind: DatabaseKind::CopiedSnapshot,
        provenance: CopiedSnapshotProvenance {
            authority_identity: "integration:copied-snapshot".to_owned(),
            staging_root: canonical_path
                .parent()
                .expect("copied fixture parent")
                .to_path_buf(),
            canonical_path,
            byte_len: metadata.len(),
            content_digest: format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
            file_identity: SnapshotFileIdentity::from_metadata(&metadata),
        },
    }
}

pub(crate) fn missing_copied_database(path: &Path) -> CopiedDatabase {
    CopiedDatabase {
        path: path.to_path_buf(),
        kind: DatabaseKind::CopiedSnapshot,
        provenance: CopiedSnapshotProvenance {
            authority_identity: "integration:missing-copied-snapshot".to_owned(),
            staging_root: path
                .parent()
                .expect("missing copied fixture parent")
                .canonicalize()
                .expect("canonicalize missing copied fixture parent"),
            canonical_path: path.to_path_buf(),
            byte_len: 0,
            content_digest: format!("sha256:{}", "0".repeat(64)),
            file_identity: SnapshotFileIdentity::Unsupported,
        },
    }
}

pub(crate) fn request_for_database(database: CopiedDatabase, command: Value) -> Value {
    json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "integration",
        "database": database,
        "command": command
    })
}

pub(crate) fn request(path: &Path, command: Value) -> Value {
    request_for_database(copied_database(path), command)
}
