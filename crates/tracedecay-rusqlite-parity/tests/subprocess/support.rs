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
            CREATE TABLE source_cursors (
                source_json TEXT NOT NULL,
                scope_json TEXT NOT NULL,
                cursor_json TEXT NOT NULL,
                PRIMARY KEY(source_json, scope_json)
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
            CREATE TABLE session_logical_copy_edges (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                copied_from_occurrence_id TEXT NOT NULL,
                proof_json TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                valid_time_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id, copied_from_occurrence_id)
            );
            CREATE TABLE session_assertions (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                assertion_id TEXT NOT NULL,
                assertion_kind TEXT NOT NULL,
                subject_anchor_id TEXT NOT NULL,
                object_anchor_id TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                valid_time_json TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                PRIMARY KEY(session_id, generation, assertion_id)
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
            CREATE TABLE session_summary_sources (
                summary_id TEXT NOT NULL,
                source_ordinal INTEGER NOT NULL,
                source_kind TEXT NOT NULL,
                source_anchor_id TEXT,
                source_summary_id TEXT,
                PRIMARY KEY(summary_id, source_ordinal)
            );
            CREATE TABLE session_summary_successors (
                predecessor_summary_id TEXT NOT NULL,
                successor_summary_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(predecessor_summary_id, successor_summary_id)
            );
            CREATE TABLE memory_v2_facts (
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                identity_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(fact_id, owner_kind, project_id)
            );
            CREATE TABLE memory_v2_assertions (
                assertion_id TEXT NOT NULL,
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                assertion_header_json TEXT NOT NULL,
                kind_json TEXT NOT NULL,
                payload_reference_json TEXT NOT NULL,
                receipt_json TEXT NOT NULL,
                asserted_at INTEGER NOT NULL,
                actor_id TEXT,
                PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id)
            );
            CREATE TABLE memory_v2_lineage_events (
                event_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL,
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                event_json TEXT NOT NULL,
                occurred_at INTEGER NOT NULL,
                recorded_at INTEGER NOT NULL
            );
            CREATE TABLE memory_v2_current_facts (
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                payload_access TEXT NOT NULL,
                trust_score REAL,
                active_assertion_id TEXT,
                last_event_id TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                retrieval_count INTEGER NOT NULL DEFAULT 0,
                access_count INTEGER NOT NULL DEFAULT 0,
                helpful_count INTEGER NOT NULL DEFAULT 0,
                unhelpful_count INTEGER NOT NULL DEFAULT 0,
                last_retrieved_at INTEGER,
                last_recalled_at INTEGER,
                last_feedback_at INTEGER,
                projection_state TEXT NOT NULL DEFAULT 'unavailable',
                vector_watermark_json TEXT,
                PRIMARY KEY(fact_id, owner_kind, project_id)
            );
            CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                anchor_json TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                projection_generation TEXT NOT NULL
            );
            CREATE TABLE generation_diagnostics (
                diagnostic_anchor TEXT PRIMARY KEY,
                generation_id TEXT NOT NULL,
                repository TEXT NOT NULL,
                worktree TEXT,
                reference TEXT,
                source_revision TEXT,
                file_occurrence_id TEXT NOT NULL,
                content_digest TEXT NOT NULL,
                symbol_occurrence_id TEXT,
                span_start INTEGER NOT NULL,
                span_end INTEGER NOT NULL,
                code TEXT NOT NULL,
                severity TEXT NOT NULL,
                message TEXT NOT NULL,
                message_digest TEXT NOT NULL,
                producer_kind TEXT NOT NULL,
                producer TEXT NOT NULL,
                analyzer_revision TEXT NOT NULL,
                configuration_revision TEXT NOT NULL,
                sanitization_receipt TEXT,
                evidence_class TEXT NOT NULL,
                collected_at INTEGER NOT NULL,
                record_state TEXT NOT NULL DEFAULT 'current',
                state_generation TEXT,
                persisted_at INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE diagnostic_generation_publications (
                generation_id TEXT PRIMARY KEY,
                record_state TEXT NOT NULL,
                state_generation TEXT,
                published_at INTEGER NOT NULL
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
            INSERT INTO source_cursors(source_json, scope_json, cursor_json) VALUES
                ('{\"source\":\"a\"}', '{\"scope\":\"1\"}', '{\"cursor\":\"1\"}'),
                ('{\"source\":\"a\"}', '{\"scope\":\"2\"}', '{\"cursor\":\"2\"}');
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
            ) VALUES (
                'session-1', 1, 'assertion-1', 'supersedes', 'anchor-1', 'anchor-2', 1,
                '{\"kind\":\"unknown\"}', '{}'
            );
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
