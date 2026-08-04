//! Fresh-store fact, assertion, evidence, and lineage schema.

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error};
use super::feedback::install_feedback_schema;

/// Installs the final memory schema into an empty database.
pub(in crate::db) async fn create_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error(operation, error))?;
    crate::db::retrieval_anchor_schema::install_retrieval_anchor_schema(conn, operation).await?;
    conn.execute_batch(
        "CREATE TABLE memory_v2_facts (
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            identity_json TEXT NOT NULL CHECK(json_valid(identity_json)),
            created_at INTEGER NOT NULL,
            PRIMARY KEY(fact_id, owner_kind, project_id),
            UNIQUE(fact_id, owner_json),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE memory_v2_assertions (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            assertion_header_json TEXT NOT NULL CHECK(json_valid(assertion_header_json)),
            kind_json TEXT NOT NULL CHECK(json_valid(kind_json)),
            payload_reference_json TEXT NOT NULL CHECK(json_valid(payload_reference_json)),
            receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
            asserted_at INTEGER NOT NULL,
            actor_id TEXT,
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id),
            UNIQUE(assertion_id, owner_json),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id)
        );

        CREATE TABLE memory_v2_assertion_supersession (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            superseded_assertion_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id, ordinal),
            UNIQUE(assertion_id, fact_id, owner_kind, project_id, superseded_assertion_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(superseded_assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id)
        );

        CREATE TABLE memory_v2_assertion_payloads (
            rowid INTEGER PRIMARY KEY AUTOINCREMENT,
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
            content TEXT NOT NULL,
            UNIQUE(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id)
        );

        CREATE VIRTUAL TABLE memory_v2_assertion_payloads_fts USING fts5(
            content,
            content='memory_v2_assertion_payloads',
            content_rowid='rowid'
        );
        CREATE TRIGGER memory_v2_payloads_fts_insert
        AFTER INSERT ON memory_v2_assertion_payloads BEGIN
            INSERT INTO memory_v2_assertion_payloads_fts(rowid, content)
            VALUES(NEW.rowid, NEW.content);
        END;
        CREATE TRIGGER memory_v2_payloads_fts_delete
        AFTER DELETE ON memory_v2_assertion_payloads BEGIN
            INSERT INTO memory_v2_assertion_payloads_fts(
                memory_v2_assertion_payloads_fts, rowid, content
            ) VALUES('delete', OLD.rowid, OLD.content);
        END;
        CREATE TRIGGER memory_v2_payloads_no_update
        BEFORE UPDATE ON memory_v2_assertion_payloads BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payloads are immutable');
        END;

        CREATE TABLE memory_v2_evidence (
            evidence_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            anchor_id TEXT NOT NULL,
            evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
            PRIMARY KEY(evidence_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(anchor_id, owner_json)
                REFERENCES retrieval_anchors(anchor_id, owner_json)
        );

        CREATE TABLE memory_v2_assertion_evidence (
            assertion_id TEXT NOT NULL,
            evidence_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id, ordinal),
            UNIQUE(assertion_id, fact_id, owner_kind, project_id, evidence_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(evidence_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_evidence(evidence_id, fact_id, owner_kind, project_id)
        );

        CREATE TABLE memory_v2_lineage_events (
            event_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            event_json TEXT NOT NULL CHECK(json_valid(event_json)),
            occurred_at INTEGER NOT NULL,
            recorded_at INTEGER NOT NULL,
            UNIQUE(event_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id)
        );

        CREATE TABLE memory_v2_current_facts (
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            payload_access TEXT NOT NULL CHECK(payload_access IN (
                'eligible', 'redacted', 'quarantined', 'retention_expired',
                'deleted', 'unavailable', 'ambiguous'
            )),
            trust_score REAL CHECK(
                trust_score IS NULL OR (trust_score >= 0.0 AND trust_score <= 1.0)
            ),
            active_assertion_id TEXT,
            last_event_id TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            retrieval_count INTEGER NOT NULL DEFAULT 0 CHECK(retrieval_count >= 0),
            access_count INTEGER NOT NULL DEFAULT 0 CHECK(access_count >= 0),
            helpful_count INTEGER NOT NULL DEFAULT 0 CHECK(helpful_count >= 0),
            unhelpful_count INTEGER NOT NULL DEFAULT 0 CHECK(unhelpful_count >= 0),
            last_retrieved_at INTEGER,
            last_recalled_at INTEGER,
            last_feedback_at INTEGER,
            projection_state TEXT NOT NULL DEFAULT 'unavailable' CHECK(projection_state IN (
                'ready', 'rebuilding', 'stale', 'unavailable'
            )),
            vector_watermark_json TEXT CHECK(
                vector_watermark_json IS NULL OR json_valid(vector_watermark_json)
            ),
            PRIMARY KEY(fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(active_assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(last_event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id)
        );

        CREATE INDEX idx_memory_v2_assertions_fact
            ON memory_v2_assertions(fact_id, owner_kind, project_id, asserted_at);
        CREATE INDEX idx_memory_v2_events_fact
            ON memory_v2_lineage_events(fact_id, owner_kind, project_id, event_sequence);
        CREATE INDEX idx_memory_v2_events_as_of
            ON memory_v2_lineage_events(
                fact_id, owner_kind, project_id, occurred_at, event_id
            );
        CREATE INDEX idx_memory_v2_current_page
            ON memory_v2_current_facts(owner_kind, project_id, fact_id);
        CREATE INDEX idx_memory_v2_evidence_anchor
            ON memory_v2_evidence(anchor_id, owner_json);
        CREATE TRIGGER memory_v2_facts_no_update
        BEFORE UPDATE ON memory_v2_facts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact identities are immutable');
        END;
        CREATE TRIGGER memory_v2_facts_no_delete
        BEFORE DELETE ON memory_v2_facts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact identities are immutable');
        END;
        CREATE TRIGGER memory_v2_assertions_no_update
        BEFORE UPDATE ON memory_v2_assertions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertions are immutable');
        END;
        CREATE TRIGGER memory_v2_assertions_no_delete
        BEFORE DELETE ON memory_v2_assertions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertions are immutable');
        END;
        CREATE TRIGGER memory_v2_supersession_no_update
        BEFORE UPDATE ON memory_v2_assertion_supersession BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion supersession is immutable');
        END;
        CREATE TRIGGER memory_v2_supersession_no_delete
        BEFORE DELETE ON memory_v2_assertion_supersession BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion supersession is immutable');
        END;
        CREATE TRIGGER memory_v2_evidence_no_update
        BEFORE UPDATE ON memory_v2_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 evidence is immutable');
        END;
        CREATE TRIGGER memory_v2_evidence_no_delete
        BEFORE DELETE ON memory_v2_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 evidence is immutable');
        END;
        CREATE TRIGGER memory_v2_assertion_evidence_no_update
        BEFORE UPDATE ON memory_v2_assertion_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion evidence is immutable');
        END;
        CREATE TRIGGER memory_v2_assertion_evidence_no_delete
        BEFORE DELETE ON memory_v2_assertion_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion evidence is immutable');
        END;
        CREATE TRIGGER memory_v2_events_no_update
        BEFORE UPDATE ON memory_v2_lineage_events BEGIN
            SELECT RAISE(ABORT, 'memory_v2 lineage events are immutable');
        END;
        CREATE TRIGGER memory_v2_events_no_delete
        BEFORE DELETE ON memory_v2_lineage_events BEGIN
            SELECT RAISE(ABORT, 'memory_v2 lineage events are immutable');
        END;
        ",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    install_feedback_schema(conn, operation).await
}
