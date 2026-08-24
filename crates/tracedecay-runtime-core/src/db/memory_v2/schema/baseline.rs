//! Exact final project-memory schema installer.

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error};
use super::automatic_facts::{
    install_automatic_fact_receipt_integrity_triggers, install_current_projection_indexes,
};
use super::final_authority::install_final_memory_support;

pub(super) const BASELINE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS memory_v2_facts (
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

        CREATE TABLE IF NOT EXISTS memory_v2_assertions (
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

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_supersession (
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

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_payloads (
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

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_v2_assertion_payloads_fts USING fts5(
            content,
            content='memory_v2_assertion_payloads',
            content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_fts_insert
        AFTER INSERT ON memory_v2_assertion_payloads BEGIN
            INSERT INTO memory_v2_assertion_payloads_fts(rowid, content)
            VALUES(NEW.rowid, NEW.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_fts_delete
        AFTER DELETE ON memory_v2_assertion_payloads BEGIN
            INSERT INTO memory_v2_assertion_payloads_fts(
                memory_v2_assertion_payloads_fts, rowid, content
            ) VALUES('delete', OLD.rowid, OLD.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_no_update
        BEFORE UPDATE ON memory_v2_assertion_payloads BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payloads are immutable');
        END;

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_payload_purges (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            payload_reference_json TEXT NOT NULL CHECK(json_valid(payload_reference_json)),
            detector_revision TEXT NOT NULL CHECK(length(detector_revision) > 0),
            purge_reason TEXT NOT NULL CHECK(purge_reason = 'detector_flagged'),
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id)
        );
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_payload_purges_no_update
        BEFORE UPDATE ON memory_v2_assertion_payload_purges BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payload purge receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_payload_purges_no_delete
        BEFORE DELETE ON memory_v2_assertion_payload_purges BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payload purge receipts are immutable');
        END;

        CREATE TABLE IF NOT EXISTS memory_v2_evidence (
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

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_evidence (
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

        CREATE TABLE IF NOT EXISTS memory_v2_lineage_events (
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

        CREATE TABLE IF NOT EXISTS memory_v2_current_facts (
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
            PRIMARY KEY(fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(active_assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(last_event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_automatic_fact_receipts (
            apply_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            idempotency_key TEXT NOT NULL,
            request_digest TEXT NOT NULL,
            request_json TEXT NOT NULL CHECK(json_valid(request_json)),
            evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
            state TEXT NOT NULL CHECK(state IN ('applied', 'quarantined')),
            quarantine_reason TEXT,
            applied_fact_id TEXT,
            applied_assertion_id TEXT,
            applied_event_id TEXT,
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY(apply_id, owner_kind, project_id),
            UNIQUE(owner_kind, project_id, idempotency_key),
            UNIQUE(owner_kind, project_id, request_digest),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            FOREIGN KEY(applied_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(applied_assertion_id, applied_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(applied_event_id, applied_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (state = 'applied'
                    AND quarantine_reason IS NULL
                    AND applied_fact_id IS NOT NULL
                    AND applied_event_id IS NOT NULL) OR
                (state = 'quarantined'
                    AND quarantine_reason IS NOT NULL
                    AND applied_fact_id IS NULL
                    AND applied_assertion_id IS NULL
                    AND applied_event_id IS NULL)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_assertions_fact
            ON memory_v2_assertions(fact_id, owner_kind, project_id, asserted_at);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_events_fact
            ON memory_v2_lineage_events(fact_id, owner_kind, project_id, event_sequence);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_events_as_of
            ON memory_v2_lineage_events(
                fact_id, owner_kind, project_id, occurred_at, event_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_current_page
            ON memory_v2_current_facts(owner_kind, project_id, fact_id);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_evidence_anchor
            ON memory_v2_evidence(anchor_id, owner_json);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_automatic_fact_receipt_list
            ON memory_v2_automatic_fact_receipts(
                owner_kind, project_id, state, recorded_at, apply_id
            );

        CREATE TRIGGER IF NOT EXISTS memory_v2_facts_no_update
        BEFORE UPDATE ON memory_v2_facts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact identities are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_facts_no_delete
        BEFORE DELETE ON memory_v2_facts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact identities are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertions_no_update
        BEFORE UPDATE ON memory_v2_assertions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertions are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertions_no_delete
        BEFORE DELETE ON memory_v2_assertions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertions are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_supersession_no_update
        BEFORE UPDATE ON memory_v2_assertion_supersession BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion supersession is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_supersession_no_delete
        BEFORE DELETE ON memory_v2_assertion_supersession BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion supersession is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_evidence_no_update
        BEFORE UPDATE ON memory_v2_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_evidence_no_delete
        BEFORE DELETE ON memory_v2_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_evidence_no_update
        BEFORE UPDATE ON memory_v2_assertion_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_evidence_no_delete
        BEFORE DELETE ON memory_v2_assertion_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_events_no_update
        BEFORE UPDATE ON memory_v2_lineage_events BEGIN
            SELECT RAISE(ABORT, 'memory_v2 lineage events are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_events_no_delete
        BEFORE DELETE ON memory_v2_lineage_events BEGIN
            SELECT RAISE(ABORT, 'memory_v2 lineage events are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_automatic_fact_receipts_no_update
        BEFORE UPDATE ON memory_v2_automatic_fact_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 automatic fact receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_automatic_fact_receipts_no_delete
        BEFORE DELETE ON memory_v2_automatic_fact_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 automatic fact receipts are immutable');
        END;";

/// Installs the only accepted project-memory persisted shape.
pub(in crate::db) async fn create_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error(operation, error))?;
    crate::db::retrieval_anchor_schema::install_retrieval_anchor_schema(conn, operation).await?;
    conn.execute_batch(BASELINE_SCHEMA)
        .await
        .map_err(|error| db_error(operation, error))?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
             source_anchor_id, owner_json, derivative_kind, derivative_id, direct_evidence
         )
         SELECT anchor_id, owner_json, 'contribution', evidence_id,
                CASE json_extract(evidence_json, '$.relation')
                    WHEN 'supports' THEN 1
                    WHEN 'contradicts' THEN 1
                    WHEN 'corrects' THEN 1
                    ELSE 0
                END
         FROM memory_v2_evidence;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
             source_anchor_id, owner_json, derivative_kind, derivative_id, direct_evidence
         )
         SELECT lineage.source_anchor_id, lineage.owner_json,
                'finding', event.event_id, 1
         FROM retrieval_anchor_reverse_lineage AS lineage
         JOIN memory_v2_evidence AS evidence
           ON evidence.anchor_id = lineage.source_anchor_id
          AND evidence.owner_json = lineage.owner_json
          AND evidence.evidence_id = lineage.derivative_id
         JOIN memory_v2_lineage_events AS event
           ON event.fact_id = evidence.fact_id
          AND event.owner_kind = evidence.owner_kind
          AND event.project_id = evidence.project_id
         WHERE lineage.derivative_kind = 'contribution'
           AND lineage.direct_evidence = 1
           AND json_extract(event.event_json, '$.kind') = 'trust_changed'
           AND COALESCE((
               SELECT disposition.state
               FROM retrieval_anchor_dispositions AS disposition
               WHERE disposition.anchor_id = lineage.source_anchor_id
                 AND disposition.owner_json = lineage.owner_json
               ORDER BY disposition.sequence DESC LIMIT 1
           ), 'active') = 'active';",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchor_derivative_tombstones (
             source_anchor_id, owner_json, derivative_kind, derivative_id,
             disposition_id, effective_at
         )
         SELECT lineage.source_anchor_id, lineage.owner_json,
                lineage.derivative_kind, lineage.derivative_id,
                current.last_event_id, current.updated_at
         FROM retrieval_anchor_reverse_lineage AS lineage
         JOIN memory_v2_evidence AS evidence
           ON evidence.anchor_id = lineage.source_anchor_id
          AND evidence.owner_json = lineage.owner_json
          AND evidence.evidence_id = lineage.derivative_id
         JOIN memory_v2_current_facts AS current
           ON current.fact_id = evidence.fact_id
          AND current.owner_kind = evidence.owner_kind
          AND current.project_id = evidence.project_id
         WHERE lineage.derivative_kind = 'contribution'
           AND current.payload_access = 'deleted';",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchor_derivative_tombstones (
             source_anchor_id, owner_json, derivative_kind, derivative_id,
             disposition_id, effective_at
         )
         SELECT lineage.source_anchor_id, lineage.owner_json,
                lineage.derivative_kind, lineage.derivative_id,
                current.last_event_id, current.updated_at
         FROM retrieval_anchor_reverse_lineage AS lineage
         JOIN memory_v2_lineage_events AS event
           ON event.event_id = lineage.derivative_id
         JOIN memory_v2_current_facts AS current
           ON current.fact_id = event.fact_id
          AND current.owner_kind = event.owner_kind
          AND current.project_id = event.project_id
         WHERE lineage.derivative_kind = 'finding'
           AND current.payload_access = 'deleted';",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    install_final_memory_support(conn, operation).await?;
    install_automatic_fact_receipt_integrity_triggers(conn, operation).await?;
    install_current_projection_indexes(conn, operation).await?;
    Ok(())
}
