//! Owner-scoped V2 fact lineage schema and bounded legacy backfill.

use std::collections::BTreeSet;

use libsql::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};
use tracedecay_domain::{
    Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactEventId, FactId,
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, FactPayloadV1, LegacyFactMappingV1, LegacyHistoryCoverageV1, PayloadAccessState,
    PayloadReferenceV1, ProvenanceId, RetentionClass, SanitizerDispositionV1, SourceStoreId,
    UtcMicros,
};

use crate::errors::{Result, TraceDecayError};
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};
use crate::tracedecay::current_timestamp;

const OPERATION: &str = "memory_v2_backfill_v1";
const MAX_BATCH_SIZE: i64 = 500;
const MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE: i64 = 512;
const RETENTION_CLASS: &str = "legacy-current-snapshot-v1";
const V1_COMPATIBILITY_SOURCE_STORE: &str = "legacy-memory-v1";
const V23_COMPATIBILITY_BANK_VECTOR_BYTES: usize = 8 + 2048 * 4;
const V23_COMPATIBILITY_BANK_VECTOR_HEADER: [u8; 8] = 2048_u64.to_le_bytes();

/// Installs only additive storage. Legacy data movement is daemon-authorized
/// and deliberately absent from bare schema creation and database open.
pub(crate) async fn create_schema(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error(operation, error))?;
    super::retrieval_anchor_schema::install_retrieval_anchor_schema(conn, operation).await?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_v2_facts (
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

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_vectors (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            vector BLOB NOT NULL,
            algebra TEXT NOT NULL,
            dimensions INTEGER NOT NULL CHECK(dimensions > 0),
            precision TEXT NOT NULL,
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertion_payloads(
                    assertion_id, fact_id, owner_kind, project_id
                ) ON DELETE CASCADE
        );

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

        CREATE TABLE IF NOT EXISTS memory_v2_legacy_map (
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            source_store_id TEXT NOT NULL,
            legacy_fact_id INTEGER NOT NULL CHECK(legacy_fact_id > 0),
            fact_id TEXT NOT NULL,
            mapping_json TEXT NOT NULL CHECK(json_valid(mapping_json)),
            PRIMARY KEY(owner_kind, project_id, source_store_id, legacy_fact_id),
            UNIQUE(fact_id, owner_kind, project_id, source_store_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_legacy_quarantine (
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL,
            source_table TEXT NOT NULL CHECK(source_table IN (
                'memory_facts', 'memory_feedback_events', 'memory_oplog'
            )),
            source_row_id INTEGER NOT NULL,
            reason_code TEXT NOT NULL,
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY(
                owner_kind, project_id, source_store_id, source_table, source_row_id
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_backfill_progress (
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            source_store_id TEXT NOT NULL,
            phase TEXT NOT NULL CHECK(phase IN (
                'feedback', 'oplog', 'facts', 'awaiting_cutover', 'cutover_complete'
            )),
            feedback_frontier INTEGER NOT NULL CHECK(feedback_frontier >= 0),
            oplog_frontier INTEGER NOT NULL CHECK(oplog_frontier >= 0),
            fact_frontier INTEGER NOT NULL CHECK(fact_frontier >= 0),
            feedback_cursor INTEGER NOT NULL DEFAULT 0 CHECK(feedback_cursor >= 0),
            oplog_cursor INTEGER NOT NULL DEFAULT 0 CHECK(oplog_cursor >= 0),
            fact_cursor INTEGER NOT NULL DEFAULT 0 CHECK(fact_cursor >= 0),
            started_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            cutover_completed_at INTEGER,
            cutover_receipt_json TEXT CHECK(
                cutover_receipt_json IS NULL OR json_valid(cutover_receipt_json)
            ),
            PRIMARY KEY(owner_kind, project_id, source_store_id),
            CHECK(
                (phase = 'cutover_complete'
                    AND cutover_completed_at IS NOT NULL
                    AND cutover_receipt_json IS NOT NULL) OR
                (phase <> 'cutover_complete'
                    AND cutover_completed_at IS NULL
                    AND cutover_receipt_json IS NULL)
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_proposals (
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            idempotency_key TEXT NOT NULL,
            request_digest TEXT NOT NULL,
            request_json TEXT NOT NULL CHECK(json_valid(request_json)),
            evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
            submitted_at INTEGER NOT NULL,
            PRIMARY KEY(proposal_id, owner_kind, project_id),
            UNIQUE(owner_kind, project_id, idempotency_key),
            UNIQUE(owner_kind, project_id, request_digest),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );
        CREATE TABLE IF NOT EXISTS memory_v2_proposal_transitions (
            transition_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            transition_id TEXT NOT NULL,
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            previous_state TEXT,
            current_state TEXT NOT NULL CHECK(current_state IN (
                'pending', 'applying', 'applied', 'rejected'
            )),
            reviewer_json TEXT CHECK(reviewer_json IS NULL OR json_valid(reviewer_json)),
            validation_json TEXT CHECK(validation_json IS NULL OR json_valid(validation_json)),
            origin TEXT NOT NULL CHECK(origin IN ('runtime', 'legacy_import')),
            promoted_fact_id TEXT,
            promoted_assertion_id TEXT,
            promoted_event_id TEXT,
            transition_json TEXT NOT NULL CHECK(json_valid(transition_json)),
            occurred_at INTEGER NOT NULL,
            UNIQUE(transition_id, proposal_id, owner_kind, project_id),
            FOREIGN KEY(proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposals(proposal_id, owner_kind, project_id),
            FOREIGN KEY(promoted_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(promoted_assertion_id, promoted_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(promoted_event_id, promoted_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(previous_state IS NULL OR previous_state IN (
                'pending', 'applying', 'applied', 'rejected'
            )),
            CHECK(
                (current_state = 'applied'
                    AND promoted_fact_id IS NOT NULL
                    AND promoted_event_id IS NOT NULL) OR
                (current_state <> 'applied'
                    AND promoted_fact_id IS NULL
                    AND promoted_assertion_id IS NULL
                    AND promoted_event_id IS NULL)
            )
        );
        CREATE TABLE IF NOT EXISTS memory_v2_proposal_current (
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN (
                'pending', 'applying', 'applied', 'rejected'
            )),
            revision INTEGER NOT NULL CHECK(revision >= 0),
            last_transition_id TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(proposal_id, owner_kind, project_id),
            FOREIGN KEY(proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposals(proposal_id, owner_kind, project_id),
            FOREIGN KEY(last_transition_id, proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposal_transitions(
                    transition_id, proposal_id, owner_kind, project_id
                )
        );
        CREATE TABLE IF NOT EXISTS memory_v2_legacy_proposal_map (
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL,
            legacy_proposal_id TEXT NOT NULL,
            proposal_id TEXT NOT NULL,
            history_coverage TEXT NOT NULL CHECK(history_coverage IN ('complete', 'unknown')),
            import_receipt_json TEXT NOT NULL CHECK(json_valid(import_receipt_json)),
            imported_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, source_store_id, legacy_proposal_id),
            UNIQUE(proposal_id, owner_kind, project_id, source_store_id),
            FOREIGN KEY(proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposals(proposal_id, owner_kind, project_id)
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
        CREATE INDEX IF NOT EXISTS idx_memory_v2_map_fact
            ON memory_v2_legacy_map(fact_id, owner_kind, project_id);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_proposal_list
            ON memory_v2_proposal_current(
                owner_kind, project_id, state, updated_at, proposal_id
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
        CREATE TRIGGER IF NOT EXISTS memory_v2_vectors_no_update
        BEFORE UPDATE ON memory_v2_assertion_vectors BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion vectors are immutable');
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
        CREATE TRIGGER IF NOT EXISTS memory_v2_map_no_update
        BEFORE UPDATE ON memory_v2_legacy_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy mappings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_map_no_delete
        BEFORE DELETE ON memory_v2_legacy_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy mappings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_quarantine_no_update
        BEFORE UPDATE ON memory_v2_legacy_quarantine BEGIN
            SELECT RAISE(ABORT, 'memory_v2 quarantine records are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_quarantine_no_delete
        BEFORE DELETE ON memory_v2_legacy_quarantine BEGIN
            SELECT RAISE(ABORT, 'memory_v2 quarantine records are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_proposals_no_update
        BEFORE UPDATE ON memory_v2_proposals BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposals are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_proposals_no_delete
        BEFORE DELETE ON memory_v2_proposals BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposals are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_proposal_transitions_no_update
        BEFORE UPDATE ON memory_v2_proposal_transitions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposal transitions are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_proposal_transitions_no_delete
        BEFORE DELETE ON memory_v2_proposal_transitions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposal transitions are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_proposal_map_no_update
        BEFORE UPDATE ON memory_v2_legacy_proposal_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy proposal mappings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_proposal_map_no_delete
        BEFORE DELETE ON memory_v2_legacy_proposal_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy proposal mappings are immutable');
        END;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    install_v20_integrity_triggers(conn, operation).await?;
    install_v21_current_projection_indexes(conn, operation).await?;
    Ok(())
}

/// Upgrades the v19 PR7 storage shape without starting a legacy-data
/// backfill.  The caller owns the enclosing exclusive migration transaction.
pub(super) async fn upgrade_v20_schema(conn: &Connection, operation: &str) -> Result<()> {
    create_schema(conn, operation).await?;

    add_column_if_missing(
        conn,
        "memory_v2_backfill_progress",
        "cutover_receipt_json",
        "cutover_receipt_json TEXT",
        operation,
    )
    .await?;
    conn.execute(
        "UPDATE memory_v2_backfill_progress SET cutover_receipt_json = json_object(
            'kind', 'legacy_v19_cutover',
            'owner_kind', owner_kind,
            'project_id', project_id,
            'source_store_id', source_store_id,
            'feedback_frontier', feedback_frontier,
            'oplog_frontier', oplog_frontier,
            'fact_frontier', fact_frontier,
            'completed_at', cutover_completed_at
         )
         WHERE phase = 'cutover_complete' AND cutover_receipt_json IS NULL",
        (),
    )
    .await
    .map_err(|error| db_error(operation, error))?;

    add_column_if_missing(
        conn,
        "memory_v2_proposals",
        "idempotency_key",
        "idempotency_key TEXT",
        operation,
    )
    .await?;
    add_column_if_missing(
        conn,
        "memory_v2_proposals",
        "request_digest",
        "request_digest TEXT",
        operation,
    )
    .await?;
    let transition_origin_added = add_column_if_missing(
        conn,
        "memory_v2_proposal_transitions",
        "origin",
        "origin TEXT NOT NULL DEFAULT 'runtime'",
        operation,
    )
    .await?;

    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memory_v2_proposals_no_update;
         DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_no_update;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    conn.execute(
        "UPDATE memory_v2_proposals
         SET idempotency_key = 'legacy-v19:' || proposal_id
         WHERE idempotency_key IS NULL OR length(idempotency_key) = 0",
        (),
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    conn.execute(
        "UPDATE memory_v2_proposals
         SET request_digest = 'legacy-v19:' || proposal_id
         WHERE request_digest IS NULL OR length(request_digest) = 0",
        (),
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    let transition_origin_backfill = if transition_origin_added {
        "UPDATE memory_v2_proposal_transitions SET origin = 'legacy_import'"
    } else {
        "UPDATE memory_v2_proposal_transitions
         SET origin = 'legacy_import'
         WHERE origin IS NULL OR origin NOT IN ('runtime', 'legacy_import')"
    };
    conn.execute(transition_origin_backfill, ())
        .await
        .map_err(|error| db_error(operation, error))?;
    rebuild_v20_proposal_transition_tables(conn, operation).await?;

    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_v2_proposals_owner_idempotency
             ON memory_v2_proposals(owner_kind, project_id, idempotency_key);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_v2_proposals_owner_request_digest
             ON memory_v2_proposals(owner_kind, project_id, request_digest);",
    )
    .await
    .map_err(|error| db_error(operation, error))?;

    scrub_payload_bearing_assertion_headers(conn, operation).await
}

/// Adds the V21 compatibility projection fields without fabricating telemetry
/// or vector readiness for already-migrated facts. The daemon-authorized
/// compatibility store is the only writer that may advance these fields.
pub(super) async fn upgrade_v21_schema(conn: &Connection, operation: &str) -> Result<()> {
    for (column, definition) in [
        (
            "retrieval_count",
            "retrieval_count INTEGER NOT NULL DEFAULT 0 CHECK(retrieval_count >= 0)",
        ),
        (
            "access_count",
            "access_count INTEGER NOT NULL DEFAULT 0 CHECK(access_count >= 0)",
        ),
        (
            "helpful_count",
            "helpful_count INTEGER NOT NULL DEFAULT 0 CHECK(helpful_count >= 0)",
        ),
        (
            "unhelpful_count",
            "unhelpful_count INTEGER NOT NULL DEFAULT 0 CHECK(unhelpful_count >= 0)",
        ),
        ("last_retrieved_at", "last_retrieved_at INTEGER"),
        ("last_recalled_at", "last_recalled_at INTEGER"),
        ("last_feedback_at", "last_feedback_at INTEGER"),
        (
            "projection_state",
            "projection_state TEXT NOT NULL DEFAULT 'unavailable' CHECK(\
                projection_state IN ('ready', 'rebuilding', 'stale', 'unavailable')\
            )",
        ),
        (
            "vector_watermark_json",
            "vector_watermark_json TEXT CHECK(\
                vector_watermark_json IS NULL OR json_valid(vector_watermark_json)\
            )",
        ),
    ] {
        add_column_if_missing(
            conn,
            "memory_v2_current_facts",
            column,
            definition,
            operation,
        )
        .await?;
    }
    create_schema(conn, operation).await
}

/// Installs V22's explicit compatibility state. V20/V21 upgrades deliberately
/// do not call this installer so their user_version remains schema-accurate.
pub(super) async fn upgrade_v22_schema(conn: &Connection, operation: &str) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await?;
    ensure_v22_proposal_schema(conn, operation).await?;
    seed_v22_feedback_history_repairs(conn, operation).await
}

/// Installs the latest V22 shape for a newly-created database. This is kept
/// separate from the V19 baseline installer because V20/V21 upgrades call the
/// baseline installer while advancing older databases.
pub(super) async fn install_v22_fresh_schema(conn: &Connection, operation: &str) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await?;
    ensure_v22_proposal_schema(conn, operation).await
}

/// V23 is deliberately additive from the already-dogfooded V22 shape: it
/// rebuilds the constrained relation projection for full V1 parity, then adds
/// owner-keyed compatibility-bank state. V22 data never relies on a silent
/// latest-schema repair at open time.
pub(super) async fn upgrade_v23_schema(conn: &Connection, operation: &str) -> Result<()> {
    upgrade_v23_fact_relation_schema(conn, operation).await?;
    install_v23_compatibility_bank_schema(conn, operation).await
}

/// Installs V23 over a fresh V22 baseline. Keeping this explicit makes a
/// newly-created database match the same V22-to-V23 contract used by durable
/// dogfood databases.
pub(super) async fn install_v23_fresh_schema(conn: &Connection, operation: &str) -> Result<()> {
    upgrade_v23_schema(conn, operation).await
}

async fn install_v22_compatibility_schema(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_v2_compatibility_operation_receipts (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            operation_id TEXT NOT NULL CHECK(length(operation_id) > 0),
            operation_kind TEXT NOT NULL CHECK(operation_kind IN (
                'add', 'update', 'remove', 'feedback', 'retrieval',
                'curation', 'merge', 'repair', 'proposal_submit',
                'proposal_reject', 'proposal_promote', 'proposal_import'
            )),
            request_digest TEXT NOT NULL CHECK(length(request_digest) > 0),
            fact_id TEXT,
            event_id TEXT,
            receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, operation_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(event_id IS NULL OR fact_id IS NOT NULL),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_legacy_feedback_event_map (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            legacy_feedback_event_id INTEGER NOT NULL
                CHECK(legacy_feedback_event_id > 0),
            fact_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            PRIMARY KEY(
                owner_kind, project_id, source_store_id, legacy_feedback_event_id
            ),
            UNIQUE(owner_kind, project_id, source_store_id, event_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_feedback_history (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action IN ('helpful', 'unhelpful')),
            old_trust REAL NOT NULL CHECK(old_trust >= 0.0 AND old_trust <= 1.0),
            new_trust REAL NOT NULL CHECK(new_trust >= 0.0 AND new_trust <= 1.0),
            occurred_at INTEGER NOT NULL,
            source TEXT,
            note TEXT,
            details_availability TEXT NOT NULL CHECK(
                details_availability IN ('available', 'legacy_redacted', 'unknown')
            ),
            PRIMARY KEY(owner_kind, project_id, fact_id, event_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            CHECK(
                details_availability = 'available' OR (source IS NULL AND note IS NULL)
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_feedback_history_repair_progress (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            feedback_frontier INTEGER NOT NULL CHECK(feedback_frontier >= 0),
            feedback_cursor INTEGER NOT NULL DEFAULT 0 CHECK(
                feedback_cursor >= 0 AND feedback_cursor <= feedback_frontier
            ),
            phase TEXT NOT NULL CHECK(phase IN ('pending', 'complete')),
            started_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            completed_at INTEGER,
            PRIMARY KEY(owner_kind, project_id, source_store_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            CHECK(
                (phase = 'complete'
                    AND feedback_cursor = feedback_frontier
                    AND completed_at IS NOT NULL) OR
                (phase = 'pending' AND completed_at IS NULL)
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_fact_relations (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_fact_id TEXT NOT NULL,
            target_fact_id TEXT NOT NULL,
            relation TEXT NOT NULL CHECK(relation IN (
                'supports', 'derived_from'
            )),
            confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
            source_label TEXT NOT NULL CHECK(
                length(source_label) > 0
                AND length(source_label) <= 4096
                AND trim(source_label) = source_label
            ),
            evidence_fact_ids_json TEXT NOT NULL CHECK(
                json_valid(evidence_fact_ids_json)
                AND json_type(evidence_fact_ids_json) = 'array'
                AND json_array_length(evidence_fact_ids_json) BETWEEN 1 AND 256
            ),
            occurred_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL CHECK(updated_at >= occurred_at),
            PRIMARY KEY(
                owner_kind, project_id, source_fact_id, target_fact_id, relation
            ),
            FOREIGN KEY(source_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(target_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            CHECK(source_fact_id <> target_fact_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE INDEX IF NOT EXISTS idx_memory_v2_compatibility_receipts_fact
            ON memory_v2_compatibility_operation_receipts(
                fact_id, owner_kind, project_id, recorded_at
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_legacy_feedback_event_map_canonical
            ON memory_v2_legacy_feedback_event_map(
                owner_kind, project_id, fact_id, event_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_feedback_history_fact
            ON memory_v2_feedback_history(
                owner_kind, project_id, fact_id, occurred_at, event_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_feedback_history_repair_pending
            ON memory_v2_feedback_history_repair_progress(
                phase, owner_kind, project_id, updated_at
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_source
            ON memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, relation, updated_at DESC
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_target
            ON memory_v2_fact_relations(
                owner_kind, project_id, target_fact_id, relation, updated_at DESC
            );
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_receipts_no_update
        BEFORE UPDATE ON memory_v2_compatibility_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_receipts_no_delete
        BEFORE DELETE ON memory_v2_compatibility_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_receipts_no_payload
        BEFORE INSERT ON memory_v2_compatibility_operation_receipts
        WHEN EXISTS (
            SELECT 1 FROM json_tree(NEW.receipt_json)
            WHERE lower(CAST(key AS TEXT)) IN (
                'content', 'payload', 'payload_json', 'metadata',
                'vector', 'vectors', 'embedding', 'embeddings',
                'vector_watermark', 'vector_watermark_json'
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility receipts cannot retain payload data');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_feedback_event_map_no_update
        BEFORE UPDATE ON memory_v2_legacy_feedback_event_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy feedback event mappings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_feedback_event_map_no_delete
        BEFORE DELETE ON memory_v2_legacy_feedback_event_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy feedback event mappings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_only_redaction
        BEFORE UPDATE ON memory_v2_feedback_history
        WHEN NOT (
            NEW.owner_kind IS OLD.owner_kind
            AND NEW.project_id IS OLD.project_id
            AND NEW.fact_id IS OLD.fact_id
            AND NEW.event_id IS OLD.event_id
            AND NEW.action IS OLD.action
            AND NEW.old_trust IS OLD.old_trust
            AND NEW.new_trust IS OLD.new_trust
            AND NEW.occurred_at IS OLD.occurred_at
            AND NEW.source IS NULL
            AND NEW.note IS NULL
            AND (
                (OLD.details_availability = 'available'
                    AND NEW.details_availability = 'legacy_redacted')
                OR (
                    OLD.source IS NULL AND OLD.note IS NULL
                    AND NEW.details_availability IS OLD.details_availability
                )
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history permits only detail redaction');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_no_delete
        BEFORE DELETE ON memory_v2_feedback_history BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history records are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_repair_progress_guard
        BEFORE UPDATE ON memory_v2_feedback_history_repair_progress
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_store_id IS NOT OLD.source_store_id
            OR NEW.owner_json IS NOT OLD.owner_json
            OR NEW.feedback_frontier IS NOT OLD.feedback_frontier
            OR NEW.feedback_cursor < OLD.feedback_cursor
            OR NEW.started_at IS NOT OLD.started_at
            OR NEW.updated_at < OLD.updated_at
            OR OLD.phase = 'complete'
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history repair progress is append-only');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_repair_progress_no_delete
        BEFORE DELETE ON memory_v2_feedback_history_repair_progress BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history repair progress is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_insert
        BEFORE INSERT ON memory_v2_fact_relations
        WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
        ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
        ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_update
        BEFORE UPDATE ON memory_v2_fact_relations
        WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
        ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
        ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_identity_guard
        BEFORE UPDATE ON memory_v2_fact_relations
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_fact_id IS NOT OLD.source_fact_id
            OR NEW.target_fact_id IS NOT OLD.target_fact_id
            OR NEW.relation IS NOT OLD.relation
            OR NEW.occurred_at IS NOT OLD.occurred_at
            OR NEW.updated_at < OLD.updated_at
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation identity is immutable');
        END;",
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

async fn upgrade_v23_fact_relation_schema(conn: &Connection, operation: &str) -> Result<()> {
    if fact_relation_schema_is_v23(conn).await? {
        return install_v23_fact_relation_support(conn, operation).await;
    }
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memory_v2_fact_relations_validate_evidence_insert;
         DROP TRIGGER IF EXISTS memory_v2_fact_relations_validate_evidence_update;
         DROP TRIGGER IF EXISTS memory_v2_fact_relations_identity_guard;
         DROP INDEX IF EXISTS idx_memory_v2_fact_relations_source;
         DROP INDEX IF EXISTS idx_memory_v2_fact_relations_target;
         ALTER TABLE memory_v2_fact_relations
         RENAME TO memory_v2_fact_relations_v22;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    create_v23_fact_relation_table(conn, operation).await?;
    conn.execute_batch(
        "INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         )
         SELECT owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, '{}', evidence_fact_ids_json,
                occurred_at, updated_at
         FROM memory_v2_fact_relations_v22;
         DROP TABLE memory_v2_fact_relations_v22;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    install_v23_fact_relation_support(conn, operation).await
}

async fn fact_relation_schema_is_v23(conn: &Connection) -> Result<bool> {
    if !table_exists(conn, "memory_v2_fact_relations").await?
        || !table_has_column(
            conn,
            "memory_v2_fact_relations",
            "provenance_json",
            OPERATION,
        )
        .await?
    {
        return Ok(false);
    }
    let Some(sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_fact_relations'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    let sql = sql.to_ascii_lowercase();
    Ok(["supports", "contradicts", "supersedes", "derived_from"]
        .into_iter()
        .all(|relation| sql.contains(&format!("'{relation}'"))))
}

async fn create_v23_fact_relation_table(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE memory_v2_fact_relations (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_fact_id TEXT NOT NULL,
            target_fact_id TEXT NOT NULL,
            relation TEXT NOT NULL CHECK(relation IN (
                'supports', 'contradicts', 'supersedes', 'derived_from'
            )),
            confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
            source_label TEXT NOT NULL CHECK(
                length(source_label) > 0
                AND length(source_label) <= 4096
                AND trim(source_label) = source_label
            ),
            provenance_json TEXT NOT NULL CHECK(
                json_valid(provenance_json)
                AND length(CAST(provenance_json AS BLOB)) <= 4096
            ),
            evidence_fact_ids_json TEXT NOT NULL CHECK(
                json_valid(evidence_fact_ids_json)
                AND json_type(evidence_fact_ids_json) = 'array'
                AND json_array_length(evidence_fact_ids_json) BETWEEN 1 AND 256
            ),
            occurred_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL CHECK(updated_at >= occurred_at),
            PRIMARY KEY(
                owner_kind, project_id, source_fact_id, target_fact_id, relation
            ),
            FOREIGN KEY(source_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(target_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            CHECK(source_fact_id <> target_fact_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );",
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

async fn install_v23_fact_relation_support(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_source
            ON memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, relation, updated_at DESC
            );
         CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_target
            ON memory_v2_fact_relations(
                owner_kind, project_id, target_fact_id, relation, updated_at DESC
            );
         CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_insert
         BEFORE INSERT ON memory_v2_fact_relations
         WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
         ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
         ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
         ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
         END;
         CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_update
         BEFORE UPDATE ON memory_v2_fact_relations
         WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
         ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
         ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
         ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
         END;
         CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_identity_guard
         BEFORE UPDATE ON memory_v2_fact_relations
         WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_fact_id IS NOT OLD.source_fact_id
            OR NEW.target_fact_id IS NOT OLD.target_fact_id
            OR NEW.relation IS NOT OLD.relation
            OR NEW.occurred_at IS NOT OLD.occurred_at
            OR NEW.updated_at < OLD.updated_at
         BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation identity is immutable');
         END;",
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

async fn install_v23_compatibility_bank_schema(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_v2_compatibility_banks (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            bank_name TEXT NOT NULL CHECK(bank_name IN (
                'all', 'general', 'user_pref', 'project', 'tool', 'decision', 'code_area'
            )),
            vector BLOB NOT NULL CHECK(
                typeof(vector) = 'blob'
                AND length(vector) = 8200
                AND substr(vector, 1, 8) = X'0008000000000000'
            ),
            hrr_algebra TEXT NOT NULL CHECK(hrr_algebra = 'amari_fhrr'),
            hrr_dim INTEGER NOT NULL CHECK(hrr_dim = 2048),
            fact_count INTEGER NOT NULL CHECK(fact_count > 0),
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, source_store_id, bank_name),
            CHECK(
                (owner_kind = 'profile'
                    AND project_id = ''
                    AND json_extract(owner_json, '$.kind') IS 'profile') OR
                (owner_kind = 'project'
                    AND project_id <> ''
                    AND json_extract(owner_json, '$.kind') IS 'project'
                    AND json_extract(owner_json, '$.project_id') IS project_id)
            )
        );
        CREATE TABLE IF NOT EXISTS memory_v2_compatibility_bank_dirty (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            bank_name TEXT NOT NULL CHECK(bank_name IN (
                'all', 'general', 'user_pref', 'project', 'tool', 'decision', 'code_area'
            )),
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, source_store_id, bank_name),
            CHECK(
                (owner_kind = 'profile'
                    AND project_id = ''
                    AND json_extract(owner_json, '$.kind') IS 'profile') OR
                (owner_kind = 'project'
                    AND project_id <> ''
                    AND json_extract(owner_json, '$.kind') IS 'project'
                    AND json_extract(owner_json, '$.project_id') IS project_id)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_compatibility_banks_owner
            ON memory_v2_compatibility_banks(
                owner_kind, project_id, source_store_id, owner_json, updated_at DESC
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_compatibility_bank_dirty_owner
            ON memory_v2_compatibility_bank_dirty(
                owner_kind, project_id, source_store_id, owner_json, updated_at ASC
            );
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_banks_identity_guard
        BEFORE UPDATE ON memory_v2_compatibility_banks
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_store_id IS NOT OLD.source_store_id
            OR NEW.owner_json IS NOT OLD.owner_json
            OR NEW.bank_name IS NOT OLD.bank_name
            OR NEW.updated_at < OLD.updated_at
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility bank identity is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_bank_dirty_identity_guard
        BEFORE UPDATE ON memory_v2_compatibility_bank_dirty
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_store_id IS NOT OLD.source_store_id
            OR NEW.owner_json IS NOT OLD.owner_json
            OR NEW.bank_name IS NOT OLD.bank_name
            OR NEW.updated_at < OLD.updated_at
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility dirty bank identity is immutable');
        END;",
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

/// Captures only the already-processed V1 feedback frontier. The repair itself
/// is deliberately daemon-driven in bounded batches after migration commits.
async fn seed_v22_feedback_history_repairs(conn: &Connection, operation: &str) -> Result<()> {
    if !table_exists(conn, "memory_v2_backfill_progress").await? {
        return Ok(());
    }
    let seeded_at = now_micros()?;
    conn.execute(
        "INSERT INTO memory_v2_feedback_history_repair_progress(
            owner_kind, project_id, source_store_id, owner_json,
            feedback_frontier, feedback_cursor, phase,
            started_at, updated_at, completed_at
         )
         SELECT owner_kind, project_id, source_store_id, owner_json,
                CASE
                    WHEN feedback_cursor < feedback_frontier THEN feedback_cursor
                    ELSE feedback_frontier
                END,
                0, 'pending', ?1, ?1, NULL
         FROM memory_v2_backfill_progress
         WHERE source_store_id = ?2
         ON CONFLICT(owner_kind, project_id, source_store_id) DO NOTHING",
        params![seeded_at, V1_COMPATIBILITY_SOURCE_STORE],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

async fn ensure_v22_proposal_schema(conn: &Connection, operation: &str) -> Result<()> {
    let current_exists = table_exists(conn, "memory_v2_proposal_current").await?;
    let transitions_exists = table_exists(conn, "memory_v2_proposal_transitions").await?;
    if !current_exists && !transitions_exists {
        // Minimal historical databases that predate the optional proposal
        // feature have no projection to rebuild. The V22 receipt/history
        // schema remains independently usable.
        return Ok(());
    }
    if !current_exists || !transitions_exists {
        return Err(db_message(
            operation,
            "proposal projection tables are only partially present",
        ));
    }
    if proposal_schema_is_v22(conn).await? {
        return Ok(());
    }
    rebuild_v22_proposal_tables(conn, operation).await
}

async fn rebuild_v22_proposal_tables(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_no_update;
         DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_no_delete;
         DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_require_origin;
         DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_no_new_applying;
         DROP INDEX IF EXISTS idx_memory_v2_proposal_list;
         ALTER TABLE memory_v2_proposal_current
         RENAME TO memory_v2_proposal_current_v21;
         ALTER TABLE memory_v2_proposal_transitions
         RENAME TO memory_v2_proposal_transitions_v21;

         CREATE TABLE memory_v2_proposal_transitions (
            transition_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            transition_id TEXT NOT NULL,
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            previous_state TEXT,
            current_state TEXT NOT NULL CHECK(current_state IN (
                'pending', 'applying', 'applied', 'rejected', 'quarantined'
            )),
            reviewer_json TEXT CHECK(reviewer_json IS NULL OR json_valid(reviewer_json)),
            validation_json TEXT CHECK(validation_json IS NULL OR json_valid(validation_json)),
            origin TEXT NOT NULL CHECK(origin IN ('runtime', 'legacy_import')),
            promoted_fact_id TEXT,
            promoted_assertion_id TEXT,
            promoted_event_id TEXT,
            transition_json TEXT NOT NULL CHECK(json_valid(transition_json)),
            occurred_at INTEGER NOT NULL,
            UNIQUE(transition_id, proposal_id, owner_kind, project_id),
            FOREIGN KEY(proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposals(proposal_id, owner_kind, project_id),
            FOREIGN KEY(promoted_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(promoted_assertion_id, promoted_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(promoted_event_id, promoted_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(previous_state IS NULL OR previous_state IN (
                'pending', 'applying', 'applied', 'rejected', 'quarantined'
            )),
            CHECK(
                (current_state = 'applied'
                    AND promoted_fact_id IS NOT NULL
                    AND promoted_event_id IS NOT NULL) OR
                (current_state <> 'applied'
                    AND promoted_fact_id IS NULL
                    AND promoted_assertion_id IS NULL
                    AND promoted_event_id IS NULL)
            )
         );
         CREATE TABLE memory_v2_proposal_current (
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN (
                'pending', 'applied', 'rejected', 'quarantined'
            )),
            revision INTEGER NOT NULL CHECK(revision >= 1),
            last_transition_id TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(proposal_id, owner_kind, project_id),
            FOREIGN KEY(proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposals(proposal_id, owner_kind, project_id),
            FOREIGN KEY(last_transition_id, proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposal_transitions(
                    transition_id, proposal_id, owner_kind, project_id
                )
         );

         INSERT INTO memory_v2_proposal_transitions(
            transition_sequence, transition_id, proposal_id, owner_kind,
            project_id, previous_state, current_state, reviewer_json,
            validation_json, origin, promoted_fact_id, promoted_assertion_id,
            promoted_event_id, transition_json, occurred_at
         )
         SELECT transition_sequence, transition_id, proposal_id, owner_kind,
                project_id, previous_state, current_state, reviewer_json,
                validation_json, origin, promoted_fact_id, promoted_assertion_id,
                promoted_event_id, transition_json, occurred_at
         FROM memory_v2_proposal_transitions_v21;
         INSERT INTO memory_v2_proposal_current(
            proposal_id, owner_kind, project_id, state, revision,
            last_transition_id, updated_at
         )
         SELECT proposal_id, owner_kind, project_id,
                CASE WHEN state = 'applying' THEN 'pending' ELSE state END,
                CASE WHEN revision < 1 THEN 1 ELSE revision END,
                last_transition_id, updated_at
         FROM memory_v2_proposal_current_v21;
         DROP TABLE memory_v2_proposal_current_v21;
         DROP TABLE memory_v2_proposal_transitions_v21;

         CREATE INDEX idx_memory_v2_proposal_list
            ON memory_v2_proposal_current(
                owner_kind, project_id, state, updated_at, proposal_id
            );
         CREATE TRIGGER memory_v2_proposal_transitions_no_update
         BEFORE UPDATE ON memory_v2_proposal_transitions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposal transitions are immutable');
         END;
         CREATE TRIGGER memory_v2_proposal_transitions_no_delete
         BEFORE DELETE ON memory_v2_proposal_transitions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposal transitions are immutable');
         END;
         CREATE TRIGGER memory_v2_proposal_transitions_require_origin
         BEFORE INSERT ON memory_v2_proposal_transitions
         WHEN NEW.origin NOT IN ('runtime', 'legacy_import')
         BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposal transition origin is invalid');
         END;
         CREATE TRIGGER memory_v2_proposal_transitions_no_new_applying
         BEFORE INSERT ON memory_v2_proposal_transitions
         WHEN NEW.previous_state = 'applying' OR NEW.current_state = 'applying'
         BEGIN
            SELECT RAISE(ABORT, 'memory_v2 proposal transitions cannot emit applying');
         END;",
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

async fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
    operation: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_xinfo(?1) WHERE name = ?2 COLLATE NOCASE",
            params![table, column],
        )
        .await
        .map_err(|error| db_error(operation, error))?;
    if rows
        .next()
        .await
        .map_err(|error| db_error(operation, error))?
        .is_some()
    {
        return Ok(false);
    }
    drop(rows);
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {definition};"))
        .await
        .map_err(|error| db_error(operation, error))?;
    Ok(true)
}

/// SQLite cannot relax a table CHECK in place. Rebuild the immutable proposal
/// transition log and its current-state projection together so v19 databases
/// retain every transition while allowing an applied, assertion-less batch.
async fn rebuild_v20_proposal_transition_tables(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_no_update;
         DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_no_delete;
         DROP TRIGGER IF EXISTS memory_v2_proposal_transitions_require_origin;
         DROP INDEX IF EXISTS idx_memory_v2_proposal_list;
         ALTER TABLE memory_v2_proposal_current
         RENAME TO memory_v2_proposal_current_v19;
         ALTER TABLE memory_v2_proposal_transitions
         RENAME TO memory_v2_proposal_transitions_v19;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;

    create_schema(conn, operation).await?;
    conn.execute_batch(
        "INSERT INTO memory_v2_proposal_transitions(
            transition_sequence, transition_id, proposal_id, owner_kind,
            project_id, previous_state, current_state, reviewer_json,
            validation_json, origin, promoted_fact_id, promoted_assertion_id,
            promoted_event_id, transition_json, occurred_at
         )
         SELECT transition_sequence, transition_id, proposal_id, owner_kind,
                project_id, previous_state, current_state, reviewer_json,
                validation_json, origin, promoted_fact_id, promoted_assertion_id,
                promoted_event_id, transition_json, occurred_at
         FROM memory_v2_proposal_transitions_v19;
         INSERT INTO memory_v2_proposal_current(
            proposal_id, owner_kind, project_id, state, revision,
            last_transition_id, updated_at
         )
         SELECT proposal_id, owner_kind, project_id, state, revision,
                last_transition_id, updated_at
         FROM memory_v2_proposal_current_v19;
         DROP TABLE memory_v2_proposal_current_v19;
         DROP TABLE memory_v2_proposal_transitions_v19;",
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

async fn install_v21_current_projection_indexes(conn: &Connection, operation: &str) -> Result<()> {
    if !table_has_column(
        conn,
        "memory_v2_current_facts",
        "projection_state",
        operation,
    )
    .await?
    {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memory_v2_current_compatibility_search
             ON memory_v2_current_facts(
                 owner_kind, project_id, updated_at DESC, fact_id
             );
         CREATE INDEX IF NOT EXISTS idx_memory_v2_current_projection_state
             ON memory_v2_current_facts(owner_kind, project_id, projection_state);",
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(operation, error))
}

async fn install_v20_integrity_triggers(conn: &Connection, operation: &str) -> Result<()> {
    let has_cutover_receipt = table_has_column(
        conn,
        "memory_v2_backfill_progress",
        "cutover_receipt_json",
        operation,
    )
    .await?;
    let has_proposal_keys =
        table_has_column(conn, "memory_v2_proposals", "idempotency_key", operation).await?
            && table_has_column(conn, "memory_v2_proposals", "request_digest", operation).await?;
    let has_transition_origin =
        table_has_column(conn, "memory_v2_proposal_transitions", "origin", operation).await?;
    if !has_cutover_receipt && !has_proposal_keys && !has_transition_origin {
        return Ok(());
    }
    let mut schema = String::new();
    if has_proposal_keys {
        schema.push_str(
            "CREATE TRIGGER IF NOT EXISTS memory_v2_proposals_require_keys
             BEFORE INSERT ON memory_v2_proposals
             WHEN NEW.idempotency_key IS NULL OR length(NEW.idempotency_key) = 0
               OR NEW.request_digest IS NULL OR length(NEW.request_digest) = 0
             BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 proposals require idempotency and request digests');
             END;",
        );
    }
    if has_transition_origin {
        schema.push_str(
            "CREATE TRIGGER IF NOT EXISTS memory_v2_proposal_transitions_require_origin
             BEFORE INSERT ON memory_v2_proposal_transitions
             WHEN NEW.origin NOT IN ('runtime', 'legacy_import')
             BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 proposal transition origin is invalid');
             END;",
        );
    }
    if has_cutover_receipt {
        schema.push_str(
            "CREATE TRIGGER IF NOT EXISTS memory_v2_backfill_progress_cutover_receipt_insert
             BEFORE INSERT ON memory_v2_backfill_progress
             WHEN (
                 NEW.phase = 'cutover_complete'
                 AND (
                     NEW.cutover_completed_at IS NULL
                     OR NEW.cutover_receipt_json IS NULL
                     OR json_valid(NEW.cutover_receipt_json) = 0
                 )
             ) OR (
                 NEW.phase <> 'cutover_complete'
                 AND (
                     NEW.cutover_completed_at IS NOT NULL
                     OR NEW.cutover_receipt_json IS NOT NULL
                 )
             )
             BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 cutover receipt does not match phase');
             END;
             CREATE TRIGGER IF NOT EXISTS memory_v2_backfill_progress_cutover_receipt_update
             BEFORE UPDATE ON memory_v2_backfill_progress
             WHEN (
                 NEW.phase = 'cutover_complete'
                 AND (
                     NEW.cutover_completed_at IS NULL
                     OR NEW.cutover_receipt_json IS NULL
                     OR json_valid(NEW.cutover_receipt_json) = 0
                 )
             ) OR (
                 NEW.phase <> 'cutover_complete'
                 AND (
                     NEW.cutover_completed_at IS NOT NULL
                     OR NEW.cutover_receipt_json IS NOT NULL
                 )
             )
             BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 cutover receipt does not match phase');
             END;",
        );
    }
    if schema.is_empty() {
        return Ok(());
    }
    conn.execute_batch(&schema)
        .await
        .map(|_| ())
        .map_err(|error| db_error(operation, error))
}

async fn table_has_column(
    conn: &Connection,
    table: &str,
    column: &str,
    operation: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_xinfo(?1) WHERE name = ?2 COLLATE NOCASE",
            params![table, column],
        )
        .await
        .map_err(|error| db_error(operation, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| db_error(operation, error))
}

async fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    row_exists(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
    )
    .await
}

async fn trigger_exists(conn: &Connection, trigger: &str) -> Result<bool> {
    row_exists(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
        params![trigger],
    )
    .await
}

/// V20/V21 retain their original feedback-backfill behavior. The V22 map and
/// history must be installed together before a backfill can write either.
async fn v22_feedback_history_schema_installed(conn: &Connection) -> Result<bool> {
    let tables = [
        "memory_v2_legacy_feedback_event_map",
        "memory_v2_feedback_history",
        "memory_v2_feedback_history_repair_progress",
    ];
    let mut present = 0;
    for table in tables {
        present += usize::from(table_exists(conn, table).await?);
    }
    match present {
        0 => Ok(false),
        count if count == tables.len() => Ok(true),
        _ => Err(db_message(
            OPERATION,
            "V22 feedback history schema is only partially present",
        )),
    }
}

async fn proposal_current_is_v22(conn: &Connection) -> Result<bool> {
    let Some(sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_proposal_current'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    let sql = sql.to_ascii_lowercase();
    Ok(sql.contains("'quarantined'")
        && !sql.contains("'applying'")
        && sql.contains("revision >= 1"))
}

async fn proposal_schema_is_v22(conn: &Connection) -> Result<bool> {
    if !proposal_current_is_v22(conn).await? {
        return Ok(false);
    }
    let Some(transitions_sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_proposal_transitions'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    Ok(transitions_sql
        .to_ascii_lowercase()
        .contains("'quarantined'")
        && trigger_exists(conn, "memory_v2_proposal_transitions_no_new_applying").await?)
}

async fn scrub_payload_bearing_assertion_headers(conn: &Connection, operation: &str) -> Result<()> {
    struct HeaderRow {
        assertion_id: String,
        fact_id: String,
        owner_kind: String,
        project_id: String,
        owner_json: String,
        kind_json: String,
        payload_reference_json: String,
        asserted_at: i64,
        actor_id: Option<String>,
    }

    let mut rows = conn
        .query(
            "SELECT assertion_id, fact_id, owner_kind, project_id, owner_json,
                    kind_json, payload_reference_json, asserted_at, actor_id
             FROM memory_v2_assertions
             WHERE json_type(assertion_header_json, '$.payload') IS NOT NULL
                OR json_type(assertion_header_json, '$.content') IS NOT NULL",
            (),
        )
        .await
        .map_err(|error| db_error(operation, error))?;
    let mut headers = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(operation, error))?
    {
        headers.push(HeaderRow {
            assertion_id: row.get(0).map_err(|error| db_error(operation, error))?,
            fact_id: row.get(1).map_err(|error| db_error(operation, error))?,
            owner_kind: row.get(2).map_err(|error| db_error(operation, error))?,
            project_id: row.get(3).map_err(|error| db_error(operation, error))?,
            owner_json: row.get(4).map_err(|error| db_error(operation, error))?,
            kind_json: row.get(5).map_err(|error| db_error(operation, error))?,
            payload_reference_json: row.get(6).map_err(|error| db_error(operation, error))?,
            asserted_at: row.get(7).map_err(|error| db_error(operation, error))?,
            actor_id: row.get(8).map_err(|error| db_error(operation, error))?,
        });
    }
    drop(rows);
    if headers.is_empty() {
        return Ok(());
    }

    conn.execute_batch("DROP TRIGGER IF EXISTS memory_v2_assertions_no_update;")
        .await
        .map_err(|error| db_error(operation, error))?;
    for header in headers {
        let owner = serde_json::from_str::<Value>(&header.owner_json)
            .map_err(|_| db_message(operation, "legacy assertion owner is not valid JSON"))?;
        let kind = serde_json::from_str::<Value>(&header.kind_json)
            .map_err(|_| db_message(operation, "legacy assertion kind is not valid JSON"))?;
        let payload_reference = serde_json::from_str::<Value>(&header.payload_reference_json)
            .map_err(|_| db_message(operation, "legacy payload reference is not valid JSON"))?;
        let mut evidence_rows = conn
            .query(
                "SELECT evidence.evidence_json
                 FROM memory_v2_assertion_evidence AS binding
                 JOIN memory_v2_evidence AS evidence
                   ON evidence.evidence_id = binding.evidence_id
                  AND evidence.fact_id = binding.fact_id
                  AND evidence.owner_kind = binding.owner_kind
                  AND evidence.project_id = binding.project_id
                 WHERE binding.assertion_id = ?1 AND binding.fact_id = ?2
                   AND binding.owner_kind = ?3 AND binding.project_id = ?4
                ORDER BY binding.ordinal",
                params![
                    header.assertion_id.as_str(),
                    header.fact_id.as_str(),
                    header.owner_kind.as_str(),
                    header.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| db_error(operation, error))?;
        let mut evidence = Vec::new();
        while let Some(row) = evidence_rows
            .next()
            .await
            .map_err(|error| db_error(operation, error))?
        {
            let encoded: String = row.get(0).map_err(|error| db_error(operation, error))?;
            evidence.push(serde_json::from_str::<Value>(&encoded).map_err(|_| {
                db_message(operation, "legacy assertion evidence is not valid JSON")
            })?);
        }
        drop(evidence_rows);
        let canonical = json!({
            "assertion_id": &header.assertion_id,
            "fact_id": &header.fact_id,
            "owner": owner,
            "kind": kind,
            "payload_reference": payload_reference,
            "evidence": evidence,
            "asserted_at": header.asserted_at,
            "actor_id": header.actor_id.as_deref(),
        });
        conn.execute(
            "UPDATE memory_v2_assertions SET assertion_header_json = ?1
             WHERE assertion_id = ?2 AND fact_id = ?3
               AND owner_kind = ?4 AND project_id = ?5",
            params![
                json_text(&canonical)?,
                header.assertion_id,
                header.fact_id,
                header.owner_kind,
                header.project_id
            ],
        )
        .await
        .map_err(|error| db_error(operation, error))?;
    }
    create_schema(conn, operation).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CapturedMemoryV2Frontiers {
    pub(crate) feedback: i64,
    pub(crate) oplog: i64,
    pub(crate) facts: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryV2BackfillBatchOutcome {
    Advanced { processed: usize },
    AwaitingCutover,
}

/// A V22 repair snapshot for legacy feedback rows that had already been
/// imported before history/map projections existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemoryV2FeedbackHistoryRepairProgress {
    pub(crate) feedback_frontier: i64,
    pub(crate) feedback_cursor: i64,
    pub(crate) complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryV2FeedbackHistoryRepairBatchOutcome {
    Advanced { processed: usize },
    Complete { processed: usize },
    NotRequired,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MemoryV2CutoverReceipt {
    receipt_id: ProvenanceId,
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    frontiers: CapturedMemoryV2Frontiers,
    dual_write_activated_at: UtcMicros,
}

impl MemoryV2CutoverReceipt {
    pub(crate) fn new(
        receipt_id: ProvenanceId,
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        frontiers: CapturedMemoryV2Frontiers,
        dual_write_activated_at: UtcMicros,
    ) -> Result<Self> {
        receipt_id
            .validate()
            .map_err(|_| db_message(OPERATION, "cutover receipt identity is invalid"))?;
        validate_scope(&owner, &source_store_id)?;
        validate_v1_compatibility_source(&source_store_id)?;
        validate_frontiers(frontiers)?;
        Ok(Self {
            receipt_id,
            owner,
            source_store_id,
            frontiers,
            dual_write_activated_at,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryV2CutoverOutcome {
    TailPending(CapturedMemoryV2Frontiers),
    Complete,
}

#[derive(Clone)]
struct OwnerKey {
    kind: &'static str,
    project_id: String,
    json: String,
}

struct Progress {
    phase: String,
    feedback_frontier: i64,
    oplog_frontier: i64,
    fact_frontier: i64,
    feedback_cursor: i64,
    oplog_cursor: i64,
    fact_cursor: i64,
    started_at: i64,
}

struct FeedbackHistoryRepairProgress {
    feedback_frontier: i64,
    feedback_cursor: i64,
    phase: String,
    started_at: i64,
}

struct CurrentFactState {
    access: PayloadAccessState,
    last_event_id: FactEventId,
    active_assertion_id: Option<FactAssertionId>,
    active_kind: Option<FactAssertionKindV1>,
    active_payload_reference: Option<PayloadReferenceV1>,
}

struct LegacyFeedback {
    event_id: i64,
    fact_id: i64,
    action: String,
    old_trust: f64,
    new_trust: f64,
    created_at: i64,
    source: Option<String>,
    note: Option<String>,
}

struct LegacyOplog {
    id: i64,
    ts: i64,
    op: String,
    fact_id: Option<i64>,
}

struct LegacyFact {
    fact_id: i64,
    content: String,
    category: String,
    tags_json: String,
    trust_score: f64,
    source: String,
    metadata_json: String,
    updated_at: i64,
    telemetry: LegacyFactTelemetry,
}

/// Usage counters carried from `memory_facts` into the canonical projection.
/// Unlike feedback, retrieval history has no legacy event log to replay, so
/// the cutover must preserve these counters or every migrated store silently
/// loses its ranking usage signal.
struct LegacyFactTelemetry {
    retrieval_count: i64,
    access_count: i64,
    helpful_count: i64,
    unhelpful_count: i64,
}

#[derive(Serialize)]
struct StoredAssertionHeaderV1<'a> {
    assertion_id: &'a FactAssertionId,
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    kind: &'a FactAssertionKindV1,
    payload_reference: &'a PayloadReferenceV1,
    evidence: &'a [tracedecay_domain::FactEvidenceRefV1],
    asserted_at: UtcMicros,
    actor_id: Option<&'a tracedecay_domain::ActorId>,
}

pub(super) async fn load_or_capture_memory_v2_frontiers(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
) -> Result<CapturedMemoryV2Frontiers> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    begin(conn, "memory_v2_capture_frontiers").await?;
    let result = async {
        let owner_key = owner_key(owner)?;
        let mut rows = conn
            .query(
                "SELECT feedback_frontier, oplog_frontier, fact_frontier
                 FROM memory_v2_backfill_progress
                 WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
                params![
                    owner_key.kind,
                    owner_key.project_id.as_str(),
                    source_store_id.as_str()
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|error| db_error(OPERATION, error))?
        {
            return Ok(CapturedMemoryV2Frontiers {
                feedback: row.get(0).map_err(|error| db_error(OPERATION, error))?,
                oplog: row.get(1).map_err(|error| db_error(OPERATION, error))?,
                facts: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            });
        }
        let frontiers = CapturedMemoryV2Frontiers {
            feedback: scalar_i64(
                conn,
                "SELECT COALESCE(MAX(event_id), 0) FROM memory_feedback_events",
            )
            .await?,
            oplog: scalar_i64(conn, "SELECT COALESCE(MAX(id), 0) FROM memory_oplog").await?,
            facts: scalar_i64(conn, "SELECT COALESCE(MAX(fact_id), 0) FROM memory_facts").await?,
        };
        let started_at = now_micros()?;
        conn.execute(
            "INSERT INTO memory_v2_backfill_progress(
                owner_kind, project_id, owner_json, source_store_id, phase,
                feedback_frontier, oplog_frontier, fact_frontier, started_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, 'feedback', ?5, ?6, ?7, ?8, ?8)",
            params![
                owner_key.kind,
                owner_key.project_id.as_str(),
                owner_key.json.as_str(),
                source_store_id.as_str(),
                frontiers.feedback,
                frontiers.oplog,
                frontiers.facts,
                started_at
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
        Ok(frontiers)
    }
    .await;
    finish_transaction(conn, result, "memory_v2_capture_frontiers").await
}

/// Processes at most one bounded source-table batch. Captured frontiers are
/// immutable job identity: retries with shifted frontiers fail closed.
pub(super) async fn backfill_memory_v2_batch(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    frontiers: CapturedMemoryV2Frontiers,
    batch_size: i64,
) -> Result<MemoryV2BackfillBatchOutcome> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    if !(1..=MAX_BATCH_SIZE).contains(&batch_size) {
        return Err(db_message(
            OPERATION,
            "backfill batch size is outside the bounded range",
        ));
    }
    if frontiers.feedback < 0 || frontiers.oplog < 0 || frontiers.facts < 0 {
        return Err(db_message(
            OPERATION,
            "backfill frontier cannot be negative",
        ));
    }
    let owner_key = owner_key(owner)?;
    begin(conn, OPERATION).await?;
    let result = async {
        let progress =
            load_or_create_progress(conn, &owner_key, source_store_id, frontiers).await?;
        match progress.phase.as_str() {
            "feedback" => {
                backfill_feedback_batch(
                    conn,
                    owner,
                    &owner_key,
                    source_store_id,
                    &progress,
                    batch_size,
                )
                .await
            }
            "oplog" => {
                backfill_oplog_batch(
                    conn,
                    owner,
                    &owner_key,
                    source_store_id,
                    &progress,
                    batch_size,
                )
                .await
            }
            "facts" => {
                backfill_fact_batch(
                    conn,
                    owner,
                    &owner_key,
                    source_store_id,
                    &progress,
                    batch_size,
                )
                .await
            }
            "awaiting_cutover" | "cutover_complete" => {
                Ok(MemoryV2BackfillBatchOutcome::AwaitingCutover)
            }
            _ => Err(db_message(OPERATION, "stored backfill phase is invalid")),
        }
    }
    .await;
    finish_transaction(conn, result, OPERATION).await
}

/// Returns the V22-owned repair snapshot for an owner/source, if that owner
/// had feedback already imported before V22 history projections existed.
pub(super) async fn feedback_history_repair_progress(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
) -> Result<Option<MemoryV2FeedbackHistoryRepairProgress>> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    let owner = owner_key(owner)?;
    let Some(progress) =
        load_feedback_history_repair_progress(conn, &owner, source_store_id).await?
    else {
        return Ok(None);
    };
    Ok(Some(MemoryV2FeedbackHistoryRepairProgress {
        feedback_frontier: progress.feedback_frontier,
        feedback_cursor: progress.feedback_cursor,
        complete: progress.phase == "complete",
    }))
}

/// Repairs at most one V22-owned, captured legacy-feedback batch. It only
/// creates mapping/history projections for lineage events V1 had already
/// imported. Rows without an owner-matched legacy mapping are excluded because
/// V1 feedback is unscoped; eligible malformed rows are quarantined and still
/// advance.
///
/// Standalone transaction wrapper retained for owner-bound batch tests; the
/// production repair tick drives `*_in_transaction` inside a caller-owned
/// authority transaction.
#[cfg(test)]
pub(super) async fn repair_memory_v2_feedback_history_batch(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<MemoryV2FeedbackHistoryRepairBatchOutcome> {
    let owner_key = feedback_history_repair_owner_key(owner, source_store_id, batch_size)?;
    begin(conn, "memory_v2_feedback_history_repair").await?;
    let result = repair_memory_v2_feedback_history_batch_inner(
        conn,
        owner,
        &owner_key,
        source_store_id,
        batch_size,
    )
    .await;
    finish_transaction(conn, result, "memory_v2_feedback_history_repair").await
}

/// Repairs one bounded V22 feedback-history batch inside the caller's
/// authoritative writer transaction. This never starts or finishes a nested
/// transaction, so the projection, V1 repair, and operation receipt can commit
/// or roll back together.
pub(super) async fn repair_memory_v2_feedback_history_batch_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<MemoryV2FeedbackHistoryRepairBatchOutcome> {
    let owner_key = feedback_history_repair_owner_key(owner, source_store_id, batch_size)?;
    repair_memory_v2_feedback_history_batch_inner(
        conn,
        owner,
        &owner_key,
        source_store_id,
        batch_size,
    )
    .await
}

fn feedback_history_repair_owner_key(
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<OwnerKey> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    if !(1..=MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE).contains(&batch_size) {
        return Err(db_message(
            OPERATION,
            "feedback history repair batch size is outside the bounded range",
        ));
    }
    owner_key(owner)
}

async fn repair_memory_v2_feedback_history_batch_inner(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<MemoryV2FeedbackHistoryRepairBatchOutcome> {
    let Some(progress) =
        load_feedback_history_repair_progress(conn, owner_key, source_store_id).await?
    else {
        return Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::NotRequired);
    };
    match progress.phase.as_str() {
        "complete" => {
            return Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 0 });
        }
        "pending" => {}
        _ => {
            return Err(db_message(
                OPERATION,
                "stored feedback repair phase is invalid",
            ));
        }
    }

    let batch = load_owner_legacy_feedback_repair_batch(
        conn,
        owner_key,
        source_store_id,
        progress.feedback_cursor,
        progress.feedback_frontier,
        batch_size,
    )
    .await?;
    if batch.is_empty() {
        complete_feedback_history_repair(
            conn,
            owner_key,
            source_store_id,
            progress.feedback_frontier,
        )
        .await?;
        return Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 0 });
    }
    for item in &batch {
        repair_legacy_feedback_history_item(
            conn,
            owner,
            owner_key,
            source_store_id,
            &progress,
            item,
        )
        .await?;
    }
    let cursor = batch
        .last()
        .map_or(progress.feedback_cursor, |item| item.event_id);
    if cursor >= progress.feedback_frontier {
        complete_feedback_history_repair(conn, owner_key, source_store_id, cursor).await?;
        Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Complete {
            processed: batch.len(),
        })
    } else {
        advance_feedback_history_repair(conn, owner_key, source_store_id, cursor).await?;
        Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced {
            processed: batch.len(),
        })
    }
}

/// Marks one owner-bound V23 compatibility-bank projection dirty inside the
/// caller's authoritative writer transaction.
pub(super) async fn mark_memory_v2_compatibility_bank_dirty_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    conn.execute(
        "INSERT INTO memory_v2_compatibility_bank_dirty(
            owner_kind, project_id, source_store_id, owner_json, bank_name, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(owner_kind, project_id, source_store_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            updated_at = max(
                excluded.updated_at,
                memory_v2_compatibility_bank_dirty.updated_at + 1
            )",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name,
            updated_at.0
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Replaces one owner-bound V23 compatibility-bank projection inside the
/// caller's authoritative writer transaction. The strict binary shape is the
/// canonical f32-2048 FHRR encoding, never a legacy global-bank payload.
pub(super) async fn upsert_memory_v2_compatibility_bank_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    vector: &[u8],
    fact_count: u64,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    if vector.len() != V23_COMPATIBILITY_BANK_VECTOR_BYTES
        || vector[..8] != V23_COMPATIBILITY_BANK_VECTOR_HEADER
    {
        return Err(db_message(
            OPERATION,
            "compatibility bank vector is not canonical f32-2048 FHRR data",
        ));
    }
    let fact_count = i64::try_from(fact_count)
        .map_err(|_| db_message(OPERATION, "compatibility bank fact count is out of range"))?;
    if fact_count == 0 {
        return Err(db_message(
            OPERATION,
            "compatibility bank fact count must be positive",
        ));
    }
    conn.execute(
        "INSERT INTO memory_v2_compatibility_banks(
            owner_kind, project_id, source_store_id, owner_json, bank_name,
            vector, hrr_algebra, hrr_dim, fact_count, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'amari_fhrr', 2048, ?7, ?8)
         ON CONFLICT(owner_kind, project_id, source_store_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            vector = excluded.vector,
            hrr_algebra = excluded.hrr_algebra,
            hrr_dim = excluded.hrr_dim,
            fact_count = excluded.fact_count,
            updated_at = excluded.updated_at
         WHERE excluded.updated_at >= memory_v2_compatibility_banks.updated_at",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name,
            vector,
            fact_count,
            updated_at.0
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Deletes an empty owner-bound V23 compatibility-bank projection inside the
/// caller's authoritative writer transaction.
pub(super) async fn delete_memory_v2_compatibility_bank_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    conn.execute(
        "DELETE FROM memory_v2_compatibility_banks
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND owner_json = ?4 AND bank_name = ?5",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Clears a V23 dirty projection only when the caller rebuilt the exact owner
/// generation it observed. A concurrent mark therefore remains pending.
pub(super) async fn clear_memory_v2_compatibility_bank_dirty_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    expected_updated_at: UtcMicros,
) -> Result<bool> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    let changed = conn
        .execute(
            "DELETE FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
               AND owner_json = ?4 AND bank_name = ?5 AND updated_at = ?6",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str(),
                owner.json.as_str(),
                bank_name,
                expected_updated_at.0
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(changed == 1)
}

fn compatibility_bank_owner_key(
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
) -> Result<OwnerKey> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    if !matches!(
        bank_name,
        "all" | "general" | "user_pref" | "project" | "tool" | "decision" | "code_area"
    ) {
        return Err(db_message(
            OPERATION,
            "compatibility bank name is unsupported",
        ));
    }
    owner_key(owner)
}

pub(super) async fn finalize_memory_v2_cutover(
    conn: &Connection,
    receipt: &MemoryV2CutoverReceipt,
) -> Result<MemoryV2CutoverOutcome> {
    validate_scope(&receipt.owner, &receipt.source_store_id)?;
    validate_v1_compatibility_source(&receipt.source_store_id)?;
    let owner = owner_key(&receipt.owner)?;
    let receipt_json = json_text(receipt)?;
    begin(conn, "memory_v2_cutover").await?;
    let result = async {
        let mut rows = conn
            .query(
                "SELECT phase, feedback_frontier, oplog_frontier, fact_frontier,
                        cutover_receipt_json
                 FROM memory_v2_backfill_progress
                 WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
                params![
                    owner.kind,
                    owner.project_id.as_str(),
                    receipt.source_store_id.as_str()
                ],
            )
            .await
            .map_err(|error| db_error("memory_v2_cutover", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| db_error("memory_v2_cutover", error))?
            .ok_or_else(|| db_message("memory_v2_cutover", "backfill progress is missing"))?;
        let phase = row
            .get::<String>(0)
            .map_err(|error| db_error("memory_v2_cutover", error))?;
        let stored = CapturedMemoryV2Frontiers {
            feedback: row
                .get(1)
                .map_err(|error| db_error("memory_v2_cutover", error))?,
            oplog: row
                .get(2)
                .map_err(|error| db_error("memory_v2_cutover", error))?,
            facts: row
                .get(3)
                .map_err(|error| db_error("memory_v2_cutover", error))?,
        };
        if phase == "cutover_complete" {
            let existing = row
                .get::<String>(4)
                .map_err(|error| db_error("memory_v2_cutover", error))?;
            canonical_cutover_replay(existing, &receipt_json)?;
            return Ok(MemoryV2CutoverOutcome::Complete);
        }
        if phase != "awaiting_cutover" {
            return Err(db_message(
                "memory_v2_cutover",
                "backfill has not reached its captured frontier",
            ));
        }
        let tail = CapturedMemoryV2Frontiers {
            feedback: scalar_i64(
                conn,
                "SELECT COALESCE(MAX(event_id), 0) FROM memory_feedback_events",
            )
            .await?,
            oplog: scalar_i64(conn, "SELECT COALESCE(MAX(id), 0) FROM memory_oplog").await?,
            facts: scalar_i64(conn, "SELECT COALESCE(MAX(fact_id), 0) FROM memory_facts").await?,
        };
        if tail.feedback > stored.feedback || tail.oplog > stored.oplog || tail.facts > stored.facts
        {
            let advanced = CapturedMemoryV2Frontiers {
                feedback: tail.feedback.max(stored.feedback),
                oplog: tail.oplog.max(stored.oplog),
                facts: tail.facts.max(stored.facts),
            };
            conn.execute(
                "UPDATE memory_v2_backfill_progress SET
                    phase = 'feedback', feedback_frontier = ?1, oplog_frontier = ?2,
                    fact_frontier = ?3, fact_cursor = 0, updated_at = ?4
                 WHERE owner_kind = ?5 AND project_id = ?6 AND source_store_id = ?7",
                params![
                    advanced.feedback,
                    advanced.oplog,
                    advanced.facts,
                    now_micros()?,
                    owner.kind,
                    owner.project_id.as_str(),
                    receipt.source_store_id.as_str()
                ],
            )
            .await
            .map_err(|error| db_error("memory_v2_cutover", error))?;
            return Ok(MemoryV2CutoverOutcome::TailPending(advanced));
        }
        if receipt.frontiers != stored {
            return Err(db_message(
                "memory_v2_cutover",
                "cutover receipt does not bind the drained frontier",
            ));
        }
        conn.execute(
            "UPDATE memory_v2_backfill_progress SET
                phase = 'cutover_complete', cutover_completed_at = ?1,
                cutover_receipt_json = ?2, updated_at = ?1
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5",
            params![
                receipt.dual_write_activated_at.0,
                receipt_json,
                owner.kind,
                owner.project_id.as_str(),
                receipt.source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error("memory_v2_cutover", error))?;
        Ok(MemoryV2CutoverOutcome::Complete)
    }
    .await;
    finish_transaction(conn, result, "memory_v2_cutover").await
}

/// Purges payload, FTS, and vector material for one exact owner/store/fact.
/// Immutable identity, assertion headers, mapping, and typed lineage remain.
///
/// Standalone transaction wrapper retained for owner-bound purge tests; the
/// production purge path drives `purge_memory_v2_fact_inner` inside a
/// caller-owned authority transaction.
#[cfg(test)]
pub(super) async fn purge_memory_v2_fact(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: &FactEventId,
    occurred_at: UtcMicros,
) -> Result<bool> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    fact_id
        .validate()
        .map_err(|_| db_message("memory_v2_purge", "fact identity is invalid"))?;
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let owner_key = owner_key(owner)?;
    begin(conn, "memory_v2_purge").await?;
    let result = purge_memory_v2_fact_inner(
        conn,
        owner,
        &owner_key,
        source_store_id,
        fact_id,
        Some(expected_last_event_id),
        occurred_at,
    )
    .await;
    let purged = finish_transaction(conn, result, "memory_v2_purge").await?;
    if purged {
        conn.execute_batch("PRAGMA incremental_vacuum(64)")
            .await
            .map_err(|error| db_error("memory_v2_purge", error))?;
    }
    Ok(purged)
}

async fn load_legacy_feedback_batch(
    conn: &Connection,
    cursor: i64,
    frontier: i64,
    limit: i64,
) -> Result<Vec<LegacyFeedback>> {
    let mut rows = conn
        .query(
            "SELECT event_id, fact_id, action, old_trust, new_trust, created_at, source, note
             FROM memory_feedback_events
             WHERE event_id > ?1 AND event_id <= ?2
             ORDER BY event_id LIMIT ?3",
            params![cursor, frontier, limit],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyFeedback {
            event_id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            fact_id: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            action: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            old_trust: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            new_trust: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            created_at: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            source: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            note: row.get(7).map_err(|error| db_error(OPERATION, error))?,
        });
    }
    Ok(batch)
}

/// V22 repair only revisits feedback whose legacy fact already belongs to the
/// exact owner. The V1 source tables are unscoped, so scanning them directly
/// would quarantine or project another owner's rows.
async fn load_owner_legacy_feedback_repair_batch(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    cursor: i64,
    frontier: i64,
    limit: i64,
) -> Result<Vec<LegacyFeedback>> {
    let mut rows = conn
        .query(
            "SELECT feedback.event_id, feedback.fact_id, feedback.action,
                    feedback.old_trust, feedback.new_trust, feedback.created_at,
                    feedback.source, feedback.note
             FROM memory_feedback_events AS feedback
             JOIN memory_v2_legacy_map AS mapping
               ON mapping.legacy_fact_id = feedback.fact_id
              AND mapping.owner_kind = ?4
              AND mapping.project_id = ?5
              AND mapping.source_store_id = ?6
             WHERE feedback.event_id > ?1 AND feedback.event_id <= ?2
             ORDER BY feedback.event_id LIMIT ?3",
            params![
                cursor,
                frontier,
                limit,
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyFeedback {
            event_id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            fact_id: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            action: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            old_trust: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            new_trust: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            created_at: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            source: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            note: row.get(7).map_err(|error| db_error(OPERATION, error))?,
        });
    }
    Ok(batch)
}

async fn backfill_feedback_batch(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    progress: &Progress,
    limit: i64,
) -> Result<MemoryV2BackfillBatchOutcome> {
    let write_v22_feedback_history = v22_feedback_history_schema_installed(conn).await?;
    let batch = load_legacy_feedback_batch(
        conn,
        progress.feedback_cursor,
        progress.feedback_frontier,
        limit,
    )
    .await?;
    if batch.is_empty() {
        update_phase(conn, owner_key, source_store_id, "oplog").await?;
        return Ok(MemoryV2BackfillBatchOutcome::Advanced { processed: 0 });
    }
    for item in &batch {
        let action = match item.action.as_str() {
            "helpful" => "helpful",
            "unhelpful" => "unhelpful",
            _ => {
                insert_quarantine(
                    conn,
                    owner_key,
                    source_store_id,
                    "memory_feedback_events",
                    item.event_id,
                    "unknown_feedback_action",
                    progress.started_at,
                )
                .await?;
                continue;
            }
        };
        let fact_id = ensure_legacy_identity(
            conn,
            owner,
            owner_key,
            source_store_id,
            item.fact_id,
            progress.started_at,
        )
        .await?;
        let previous = Confidence::new(item.old_trust);
        let current = Confidence::new(item.new_trust);
        let occurred_at = seconds_to_micros(item.created_at);
        let (Ok(previous), Ok(current), Some(occurred_at)) = (previous, current, occurred_at)
        else {
            insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "invalid_feedback_contract",
                progress.started_at,
            )
            .await?;
            continue;
        };
        if previous == current {
            insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "non_transition_feedback",
                progress.started_at,
            )
            .await?;
            continue;
        }
        if (action == "helpful" && current <= previous)
            || (action == "unhelpful" && current >= previous)
        {
            insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "feedback_action_direction_mismatch",
                progress.started_at,
            )
            .await?;
            continue;
        }
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous,
                current,
                evidence_ids: Vec::new(),
            },
            occurred_at,
            None,
        )
        .map_err(|_| db_message(OPERATION, "typed feedback event construction failed"))?;
        if write_v22_feedback_history {
            if !legacy_feedback_mapping_can_be_recorded(
                conn,
                owner_key,
                source_store_id,
                item.event_id,
                &fact_id,
                event.event_id(),
                progress.started_at,
            )
            .await?
            {
                continue;
            }
        }
        insert_event(conn, owner_key, &event, progress.started_at).await?;
        if write_v22_feedback_history {
            let (source, note, details_availability, quarantine_reason) =
                sanitize_legacy_feedback_details(item.source.as_deref(), item.note.as_deref());
            if let Some(reason) = quarantine_reason {
                insert_quarantine(
                    conn,
                    owner_key,
                    source_store_id,
                    "memory_feedback_events",
                    item.event_id,
                    reason,
                    progress.started_at,
                )
                .await?;
            }
            insert_legacy_feedback_event_mapping(
                conn,
                owner_key,
                source_store_id,
                item.event_id,
                &fact_id,
                event.event_id(),
            )
            .await?;
            insert_feedback_history(
                conn,
                owner_key,
                &fact_id,
                event.event_id(),
                action,
                previous,
                current,
                occurred_at,
                source.as_deref(),
                note.as_deref(),
                details_availability,
            )
            .await?;
        }
        update_current(
            conn,
            owner_key,
            &fact_id,
            None,
            Some(current.as_f64()),
            event.event_id(),
            occurred_at.0,
        )
        .await?;
    }
    let cursor = batch
        .last()
        .map_or(progress.feedback_cursor, |item| item.event_id);
    update_cursor(conn, owner_key, source_store_id, "feedback_cursor", cursor).await?;
    Ok(MemoryV2BackfillBatchOutcome::Advanced {
        processed: batch.len(),
    })
}

async fn repair_legacy_feedback_history_item(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    progress: &FeedbackHistoryRepairProgress,
    item: &LegacyFeedback,
) -> Result<()> {
    let action = match item.action.as_str() {
        "helpful" => "helpful",
        "unhelpful" => "unhelpful",
        _ => {
            return insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "unknown_feedback_action",
                progress.started_at,
            )
            .await;
        }
    };
    let (Ok(previous), Ok(current), Some(occurred_at)) = (
        Confidence::new(item.old_trust),
        Confidence::new(item.new_trust),
        seconds_to_micros(item.created_at),
    ) else {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "invalid_feedback_contract",
            progress.started_at,
        )
        .await;
    };
    if previous == current {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "non_transition_feedback",
            progress.started_at,
        )
        .await;
    }
    if (action == "helpful" && current <= previous)
        || (action == "unhelpful" && current >= previous)
    {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_action_direction_mismatch",
            progress.started_at,
        )
        .await;
    }
    let Some(mapped_fact_id) = optional_string(
        conn,
        "SELECT fact_id FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND legacy_fact_id = ?4",
        params![
            owner_key.kind,
            owner_key.project_id.as_str(),
            source_store_id.as_str(),
            item.fact_id
        ],
    )
    .await?
    else {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_mapping_unavailable",
            progress.started_at,
        )
        .await;
    };
    let fact_id = match FactId::new(mapped_fact_id) {
        Ok(fact_id) => fact_id,
        Err(_) => {
            return insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "feedback_mapping_invalid",
                progress.started_at,
            )
            .await;
        }
    };
    let event = match FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::TrustChanged {
            previous,
            current,
            evidence_ids: Vec::new(),
        },
        occurred_at,
        None,
    ) {
        Ok(event) => event,
        Err(_) => {
            return insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "feedback_lineage_unavailable",
                progress.started_at,
            )
            .await;
        }
    };
    if !row_exists(
        conn,
        "SELECT 1 FROM memory_v2_lineage_events
         WHERE event_id = ?1 AND fact_id = ?2 AND owner_kind = ?3 AND project_id = ?4",
        params![
            event.event_id().as_str(),
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await?
    {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_lineage_unavailable",
            progress.started_at,
        )
        .await;
    }
    if !legacy_feedback_mapping_can_be_recorded(
        conn,
        owner_key,
        source_store_id,
        item.event_id,
        &fact_id,
        event.event_id(),
        progress.started_at,
    )
    .await?
    {
        return Ok(());
    }
    let (source, note, details_availability, quarantine_reason) =
        sanitize_legacy_feedback_details(item.source.as_deref(), item.note.as_deref());
    if let Some(reason) = quarantine_reason {
        insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            reason,
            progress.started_at,
        )
        .await?;
    }
    insert_legacy_feedback_event_mapping(
        conn,
        owner_key,
        source_store_id,
        item.event_id,
        &fact_id,
        event.event_id(),
    )
    .await?;
    insert_feedback_history(
        conn,
        owner_key,
        &fact_id,
        event.event_id(),
        action,
        previous,
        current,
        occurred_at,
        source.as_deref(),
        note.as_deref(),
        details_availability,
    )
    .await
}

async fn backfill_oplog_batch(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    progress: &Progress,
    limit: i64,
) -> Result<MemoryV2BackfillBatchOutcome> {
    let mut rows = conn
        .query(
            "SELECT id, ts, op, fact_id FROM memory_oplog
             WHERE id > ?1 AND id <= ?2 ORDER BY id LIMIT ?3",
            params![progress.oplog_cursor, progress.oplog_frontier, limit],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyOplog {
            id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            ts: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            op: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            fact_id: row.get(3).map_err(|error| db_error(OPERATION, error))?,
        });
    }
    if batch.is_empty() {
        update_phase(conn, owner_key, source_store_id, "facts").await?;
        return Ok(MemoryV2BackfillBatchOutcome::Advanced { processed: 0 });
    }
    for item in &batch {
        let Some(legacy_fact_id) = item.fact_id else {
            continue;
        };
        let fact_id = ensure_legacy_identity(
            conn,
            owner,
            owner_key,
            source_store_id,
            legacy_fact_id,
            progress.started_at,
        )
        .await?;
        match item.op.as_str() {
            "remove" => {
                let Some(occurred_at) = seconds_to_micros(item.ts) else {
                    insert_quarantine(
                        conn,
                        owner_key,
                        source_store_id,
                        "memory_oplog",
                        item.id,
                        "invalid_oplog_timestamp",
                        progress.started_at,
                    )
                    .await?;
                    continue;
                };
                purge_memory_v2_fact_inner(
                    conn,
                    owner,
                    owner_key,
                    source_store_id,
                    &fact_id,
                    None,
                    occurred_at,
                )
                .await?;
            }
            "add" | "update" => {
                insert_quarantine(
                    conn,
                    owner_key,
                    source_store_id,
                    "memory_oplog",
                    item.id,
                    "mutation_requires_snapshot_replay",
                    progress.started_at,
                )
                .await?;
            }
            "feedback" => {
                insert_quarantine(
                    conn,
                    owner_key,
                    source_store_id,
                    "memory_oplog",
                    item.id,
                    "feedback_detail_withheld",
                    progress.started_at,
                )
                .await?;
            }
            "curate" => {
                insert_quarantine(
                    conn,
                    owner_key,
                    source_store_id,
                    "memory_oplog",
                    item.id,
                    "curation_detail_withheld",
                    progress.started_at,
                )
                .await?;
            }
            _ => {
                insert_quarantine(
                    conn,
                    owner_key,
                    source_store_id,
                    "memory_oplog",
                    item.id,
                    "unsupported_oplog_operation",
                    progress.started_at,
                )
                .await?;
            }
        }
    }
    let cursor = batch.last().map_or(progress.oplog_cursor, |item| item.id);
    update_cursor(conn, owner_key, source_store_id, "oplog_cursor", cursor).await?;
    Ok(MemoryV2BackfillBatchOutcome::Advanced {
        processed: batch.len(),
    })
}

async fn backfill_fact_batch(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    progress: &Progress,
    limit: i64,
) -> Result<MemoryV2BackfillBatchOutcome> {
    let mut rows = conn
        .query(
            "SELECT fact_id, content, category, tags, trust_score, source, metadata, updated_at,
                    retrieval_count, access_count, helpful_count, unhelpful_count
             FROM memory_facts
             WHERE fact_id > ?1 AND fact_id <= ?2 ORDER BY fact_id LIMIT ?3",
            params![progress.fact_cursor, progress.fact_frontier, limit],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyFact {
            fact_id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            content: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            category: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            tags_json: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            trust_score: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            source: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            metadata_json: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            updated_at: row.get(7).map_err(|error| db_error(OPERATION, error))?,
            telemetry: LegacyFactTelemetry {
                retrieval_count: row.get(8).map_err(|error| db_error(OPERATION, error))?,
                access_count: row.get(9).map_err(|error| db_error(OPERATION, error))?,
                helpful_count: row.get(10).map_err(|error| db_error(OPERATION, error))?,
                unhelpful_count: row.get(11).map_err(|error| db_error(OPERATION, error))?,
            },
        });
    }
    if batch.is_empty() {
        update_phase(conn, owner_key, source_store_id, "awaiting_cutover").await?;
        return Ok(MemoryV2BackfillBatchOutcome::AwaitingCutover);
    }
    for legacy in &batch {
        let fact_id = ensure_legacy_identity(
            conn,
            owner,
            owner_key,
            source_store_id,
            legacy.fact_id,
            progress.started_at,
        )
        .await?;
        if let Err(reason) = backfill_fact_payload(
            conn,
            owner,
            owner_key,
            source_store_id,
            &fact_id,
            legacy,
            progress.started_at,
        )
        .await?
        {
            quarantine_fact(
                conn,
                owner,
                owner_key,
                source_store_id,
                &fact_id,
                legacy.fact_id,
                reason,
                progress.started_at,
            )
            .await?;
        }
    }
    let cursor = batch
        .last()
        .map_or(progress.fact_cursor, |item| item.fact_id);
    update_cursor(conn, owner_key, source_store_id, "fact_cursor", cursor).await?;
    Ok(MemoryV2BackfillBatchOutcome::Advanced {
        processed: batch.len(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn backfill_fact_payload(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    _source_store_id: &SourceStoreId,
    fact_id: &FactId,
    legacy: &LegacyFact,
    recorded_at: i64,
) -> Result<std::result::Result<(), &'static str>> {
    let Some(asserted_at) = seconds_to_micros(legacy.updated_at) else {
        return Ok(Err("invalid_fact_timestamp"));
    };
    let Ok(trust) = Confidence::new(legacy.trust_score) else {
        return Ok(Err("invalid_fact_trust"));
    };
    let Ok(category) = parse_category(&legacy.category) else {
        return Ok(Err("invalid_fact_category"));
    };
    let Ok(mut tags) = serde_json::from_str::<Vec<String>>(&legacy.tags_json) else {
        return Ok(Err("invalid_fact_tags"));
    };
    let Ok(metadata) = serde_json::from_str::<Value>(&legacy.metadata_json) else {
        return Ok(Err("invalid_fact_metadata"));
    };
    let mut entities = load_legacy_entities(conn, legacy.fact_id).await?;
    tags.sort_unstable();
    entities.sort_unstable();
    let original = json!({
        "content": legacy.content,
        "category": category_label(category),
        "tags": tags,
        "entities": entities,
        "metadata": metadata
    });
    let sanitized = sanitize_memory_fact_payload(original)
        .map_err(|_| db_message(OPERATION, "fact privacy sanitizer failed"))?;
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitized else {
        return Ok(Err("fact_payload_quarantined"));
    };
    let Some(source) = sanitize_provider_metadata_text(&legacy.source) else {
        return Ok(Err("fact_source_quarantined"));
    };
    let Some(content) = payload.get("content").and_then(Value::as_str) else {
        return Ok(Err("sanitized_fact_content_invalid"));
    };
    let Some(tags) = payload.get("tags").and_then(value_strings) else {
        return Ok(Err("sanitized_fact_tags_invalid"));
    };
    let Some(entities) = payload.get("entities").and_then(value_strings) else {
        return Ok(Err("sanitized_fact_entities_invalid"));
    };
    let Some(metadata) = payload.get("metadata").cloned() else {
        return Ok(Err("sanitized_fact_metadata_invalid"));
    };
    let Ok(retention) = RetentionClass::new(RETENTION_CLASS) else {
        return Err(db_message(
            OPERATION,
            "retention class configuration is invalid",
        ));
    };
    let Ok(fact_payload) = FactPayloadV1::new(
        content.to_owned(),
        category,
        tags.clone(),
        entities.clone(),
        metadata.clone(),
        receipt,
        retention,
    ) else {
        return Ok(Err("sanitized_fact_contract_invalid"));
    };
    let payload_reference = fact_payload
        .payload_reference()
        .map_err(|_| db_message(OPERATION, "typed payload reference construction failed"))?;
    let current = current_fact_state(conn, owner_key, fact_id).await?;
    let assertion_kind = match current.active_assertion_id.as_ref() {
        Some(_) if current.active_payload_reference.as_ref() == Some(&payload_reference) => current
            .active_kind
            .clone()
            .ok_or_else(|| db_message(OPERATION, "active assertion kind is missing"))?,
        Some(active) => FactAssertionKindV1::Correction {
            supersedes: active.clone(),
        },
        None => FactAssertionKindV1::LegacyImport,
    };
    let Ok(assertion) = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        assertion_kind,
        fact_payload,
        Vec::new(),
        asserted_at,
        None,
    ) else {
        return Ok(Err("typed_assertion_invalid"));
    };
    insert_assertion(conn, owner_key, &assertion).await?;
    let assertion_event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        asserted_at,
        None,
    )
    .map_err(|_| db_message(OPERATION, "typed assertion event construction failed"))?;
    insert_event(conn, owner_key, &assertion_event, recorded_at).await?;
    let access = match assertion.payload().receipt().disposition() {
        SanitizerDispositionV1::Accepted => PayloadAccessState::Eligible,
        SanitizerDispositionV1::Redacted => PayloadAccessState::Redacted,
        SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
            return Ok(Err("durable_receipt_disposition_invalid"));
        }
    };
    let last_event_id = if current.access == access {
        assertion_event.event_id().clone()
    } else {
        let access_event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: current.access,
                current: access,
            },
            asserted_at,
            None,
        )
        .map_err(|_| db_message(OPERATION, "typed payload access event construction failed"))?;
        insert_event(conn, owner_key, &access_event, recorded_at).await?;
        access_event.event_id().clone()
    };
    update_current(
        conn,
        owner_key,
        fact_id,
        Some((assertion.assertion_id(), access)),
        Some(trust.as_f64()),
        &last_event_id,
        asserted_at.0,
    )
    .await?;
    merge_legacy_fact_telemetry(conn, owner_key, fact_id, &legacy.telemetry).await?;
    mirror_sanitized_legacy(
        conn,
        SanitizedLegacyMirror {
            legacy_fact_id: legacy.fact_id,
            content,
            category: category_label(category),
            tags: &tags,
            metadata: &metadata,
            entities: &entities,
            source: &source,
            invalidate_vector: access == PayloadAccessState::Redacted,
        },
    )
    .await?;
    Ok(Ok(()))
}

async fn ensure_legacy_identity(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    legacy_fact_id: i64,
    migrated_at: i64,
) -> Result<FactId> {
    let material = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Legacy {
            source_store_id: source_store_id.clone(),
            legacy_fact_id,
        },
    )
    .map_err(|_| db_message(OPERATION, "typed legacy identity construction failed"))?;
    let fact_id = FactId::derive(&material)
        .map_err(|_| db_message(OPERATION, "typed fact identity derivation failed"))?;
    let identity_json = json_text(&material)?;
    insert_fact_identity(conn, owner_key, &fact_id, &identity_json, migrated_at).await?;
    let mapping = LegacyFactMappingV1::new(
        owner.clone(),
        source_store_id.clone(),
        legacy_fact_id,
        fact_id.clone(),
        LegacyHistoryCoverageV1::Unknown,
        UtcMicros(migrated_at),
    )
    .map_err(|_| db_message(OPERATION, "typed legacy mapping construction failed"))?;
    insert_mapping(conn, owner_key, &mapping).await?;
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::LegacyImported { mapping },
        UtcMicros(migrated_at),
        None,
    )
    .map_err(|_| db_message(OPERATION, "typed legacy import event construction failed"))?;
    insert_event(conn, owner_key, &event, migrated_at).await?;
    ensure_current(conn, owner_key, &fact_id, event.event_id(), migrated_at).await?;
    Ok(fact_id)
}

async fn insert_fact_identity(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    identity_json: &str,
    created_at: i64,
) -> Result<()> {
    if let Some(existing) = optional_string(
        conn,
        "SELECT identity_json FROM memory_v2_facts WHERE fact_id = ?1",
        params![fact_id.as_str()],
    )
    .await?
    {
        return canonical_replay(existing, identity_json, "fact identity");
    }
    conn.execute(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            identity_json,
            created_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn insert_mapping(
    conn: &Connection,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> Result<()> {
    let mapping_json = json_text(mapping)?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT mapping_json FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2
           AND source_store_id = ?3 AND legacy_fact_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            mapping.source_store_id().as_str(),
            mapping.legacy_fact_id()
        ],
    )
    .await?
    {
        return canonical_replay(existing, &mapping_json, "legacy mapping");
    }
    conn.execute(
        "INSERT INTO memory_v2_legacy_map(
            owner_kind, project_id, owner_json, source_store_id,
            legacy_fact_id, fact_id, mapping_json
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            mapping.source_store_id().as_str(),
            mapping.legacy_fact_id(),
            mapping.fact_id().as_str(),
            mapping_json
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_legacy_feedback_event_mapping(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    legacy_feedback_event_id: i64,
    fact_id: &FactId,
    event_id: &FactEventId,
) -> Result<()> {
    validate_v1_compatibility_source(source_store_id)?;
    let mut rows = conn
        .query(
            "SELECT fact_id, event_id FROM memory_v2_legacy_feedback_event_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_feedback_event_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str(),
                legacy_feedback_event_id
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let existing_fact_id = row
            .get::<String>(0)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_event_id = row
            .get::<String>(1)
            .map_err(|error| db_error(OPERATION, error))?;
        if existing_fact_id == fact_id.as_str() && existing_event_id == event_id.as_str() {
            return Ok(());
        }
        return Err(db_message(
            OPERATION,
            "legacy feedback event mapping identity collision",
        ));
    }
    drop(rows);
    if let Some(existing_legacy_id) = optional_i64(
        conn,
        "SELECT legacy_feedback_event_id FROM memory_v2_legacy_feedback_event_map
         WHERE owner_kind = ?1 AND project_id = ?2
           AND source_store_id = ?3 AND event_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            event_id.as_str()
        ],
    )
    .await?
    {
        if existing_legacy_id == legacy_feedback_event_id {
            return Ok(());
        }
        return Err(db_message(
            OPERATION,
            "canonical feedback event maps to a different legacy event",
        ));
    }
    conn.execute(
        "INSERT INTO memory_v2_legacy_feedback_event_map(
            owner_kind, project_id, source_store_id, legacy_feedback_event_id, fact_id, event_id
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            legacy_feedback_event_id,
            fact_id.as_str(),
            event_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

/// Conflicting legacy numeric rows must not turn a resumable V22 repair or
/// V1 backfill into a permanent error. The first canonical mapping wins;
/// divergent replays are quarantined while the caller advances its cursor.
#[allow(clippy::too_many_arguments)]
async fn legacy_feedback_mapping_can_be_recorded(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    legacy_feedback_event_id: i64,
    fact_id: &FactId,
    event_id: &FactEventId,
    recorded_at: i64,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT fact_id, event_id FROM memory_v2_legacy_feedback_event_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_feedback_event_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str(),
                legacy_feedback_event_id
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let existing_fact_id = row
            .get::<String>(0)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_event_id = row
            .get::<String>(1)
            .map_err(|error| db_error(OPERATION, error))?;
        if existing_fact_id != fact_id.as_str() || existing_event_id != event_id.as_str() {
            insert_quarantine(
                conn,
                owner,
                source_store_id,
                "memory_feedback_events",
                legacy_feedback_event_id,
                "feedback_mapping_collision",
                recorded_at,
            )
            .await?;
            return Ok(false);
        }
    }
    drop(rows);
    if let Some(existing_legacy_event_id) = optional_i64(
        conn,
        "SELECT legacy_feedback_event_id FROM memory_v2_legacy_feedback_event_map
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND event_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            event_id.as_str()
        ],
    )
    .await?
    {
        if existing_legacy_event_id != legacy_feedback_event_id {
            insert_quarantine(
                conn,
                owner,
                source_store_id,
                "memory_feedback_events",
                legacy_feedback_event_id,
                "feedback_event_duplicate",
                recorded_at,
            )
            .await?;
            return Ok(false);
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn insert_feedback_history(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    event_id: &FactEventId,
    action: &str,
    old_trust: Confidence,
    new_trust: Confidence,
    occurred_at: UtcMicros,
    source: Option<&str>,
    note: Option<&str>,
    details_availability: &str,
) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT action, old_trust, new_trust, occurred_at, source, note, details_availability
             FROM memory_v2_feedback_history
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3 AND event_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                fact_id.as_str(),
                event_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let existing_action = row
            .get::<String>(0)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_old_trust = row
            .get::<f64>(1)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_new_trust = row
            .get::<f64>(2)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_occurred_at = row
            .get::<i64>(3)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_source = row
            .get::<Option<String>>(4)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_note = row
            .get::<Option<String>>(5)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_availability = row
            .get::<String>(6)
            .map_err(|error| db_error(OPERATION, error))?;
        if existing_action != action
            || existing_old_trust != old_trust.as_f64()
            || existing_new_trust != new_trust.as_f64()
            || existing_occurred_at != occurred_at.0
        {
            return Err(db_message(OPERATION, "feedback history identity collision"));
        }
        if existing_source.as_deref() == source
            && existing_note.as_deref() == note
            && existing_availability == details_availability
        {
            return Ok(());
        }
        if existing_source.is_none()
            && existing_note.is_none()
            && matches!(
                existing_availability.as_str(),
                "legacy_redacted" | "unknown"
            )
        {
            return Ok(());
        }
        return Err(db_message(OPERATION, "feedback history detail collision"));
    }
    drop(rows);
    conn.execute(
        "INSERT INTO memory_v2_feedback_history(
            owner_kind, project_id, fact_id, event_id, action, old_trust, new_trust,
            occurred_at, source, note, details_availability
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            fact_id.as_str(),
            event_id.as_str(),
            action,
            old_trust.as_f64(),
            new_trust.as_f64(),
            occurred_at.0,
            source,
            note,
            details_availability
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn insert_event(
    conn: &Connection,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
    recorded_at: i64,
) -> Result<()> {
    let event_json = json_text(event)?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT event_json FROM memory_v2_lineage_events WHERE event_id = ?1",
        params![event.event_id().as_str()],
    )
    .await?
    {
        return canonical_replay(existing, &event_json, "lineage event");
    }
    conn.execute(
        "INSERT INTO memory_v2_lineage_events(
            event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            event.event_id().as_str(),
            event.fact_id().as_str(),
            owner.kind,
            owner.project_id.as_str(),
            event_json,
            event.occurred_at().0,
            recorded_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn insert_assertion(
    conn: &Connection,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> Result<()> {
    let payload_reference = assertion
        .payload()
        .payload_reference()
        .map_err(|_| db_message(OPERATION, "typed payload reference construction failed"))?;
    let header = StoredAssertionHeaderV1 {
        assertion_id: assertion.assertion_id(),
        fact_id: assertion.fact_id(),
        owner: assertion.owner(),
        kind: assertion.kind(),
        payload_reference: &payload_reference,
        evidence: assertion.evidence(),
        asserted_at: assertion.asserted_at(),
        actor_id: assertion.actor_id(),
    };
    let header_json = json_text(&header)?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT assertion_header_json FROM memory_v2_assertions WHERE assertion_id = ?1",
        params![assertion.assertion_id().as_str()],
    )
    .await?
    {
        canonical_replay(existing, &header_json, "assertion")?;
    } else {
        conn.execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                header_json,
                json_text(assertion.kind())?,
                json_text(&payload_reference)?,
                json_text(assertion.payload().receipt())?,
                assertion.asserted_at().0,
                assertion.actor_id().map(|actor| actor.as_str())
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    insert_assertion_supersession(conn, owner, assertion).await?;
    insert_assertion_evidence(conn, owner, assertion).await?;
    let payload_json = json_text(assertion.payload())?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT payload_json FROM memory_v2_assertion_payloads WHERE assertion_id = ?1",
        params![assertion.assertion_id().as_str()],
    )
    .await?
    {
        canonical_replay(existing, &payload_json, "assertion payload")?;
    } else {
        conn.execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                payload_json,
                assertion.payload().content()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

async fn insert_assertion_supersession(
    conn: &Connection,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> Result<()> {
    let superseded: Vec<&FactAssertionId> = match assertion.kind() {
        FactAssertionKindV1::Correction { supersedes } => vec![supersedes],
        FactAssertionKindV1::Merge { supersedes } => supersedes.iter().collect(),
        FactAssertionKindV1::Initial | FactAssertionKindV1::LegacyImport => Vec::new(),
    };
    for (ordinal, superseded_id) in superseded.iter().enumerate() {
        let existing = optional_string(
            conn,
            "SELECT superseded_assertion_id
             FROM memory_v2_assertion_supersession
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 AND ordinal = ?5",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                ordinal as i64
            ],
        )
        .await?;
        if let Some(existing) = existing {
            canonical_replay(existing, superseded_id.as_str(), "assertion supersession")?;
        } else {
            conn.execute(
                "INSERT INTO memory_v2_assertion_supersession(
                    assertion_id, fact_id, owner_kind, project_id,
                    superseded_assertion_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    superseded_id.as_str(),
                    ordinal as i64
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        }
    }
    let child_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*) FROM memory_v2_assertion_supersession
         WHERE assertion_id = ?1 AND fact_id = ?2
           AND owner_kind = ?3 AND project_id = ?4",
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await?;
    if child_count != superseded.len() as i64 {
        return Err(db_message(
            OPERATION,
            "assertion supersession child collision",
        ));
    }
    Ok(())
}

async fn insert_assertion_evidence(
    conn: &Connection,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> Result<()> {
    for (ordinal, evidence) in assertion.evidence().iter().enumerate() {
        let evidence_json = json_text(evidence)?;
        if let Some(existing) = optional_string(
            conn,
            "SELECT evidence_json FROM memory_v2_evidence WHERE evidence_id = ?1",
            params![evidence.evidence_id().as_str()],
        )
        .await?
        {
            canonical_replay(existing, &evidence_json, "fact evidence")?;
        } else {
            conn.execute(
                "INSERT INTO memory_v2_evidence(
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    evidence.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    owner.json.as_str(),
                    evidence.anchor_id().as_str(),
                    evidence_json
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        }
        let existing = optional_string(
            conn,
            "SELECT evidence_id FROM memory_v2_assertion_evidence
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 AND ordinal = ?5",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                ordinal as i64
            ],
        )
        .await?;
        if let Some(existing) = existing {
            canonical_replay(
                existing,
                evidence.evidence_id().as_str(),
                "assertion evidence",
            )?;
        } else {
            conn.execute(
                "INSERT INTO memory_v2_assertion_evidence(
                    assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    ordinal as i64
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        }
    }
    let child_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*) FROM memory_v2_assertion_evidence
         WHERE assertion_id = ?1 AND fact_id = ?2
           AND owner_kind = ?3 AND project_id = ?4",
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await?;
    if child_count != assertion.evidence().len() as i64 {
        return Err(db_message(OPERATION, "assertion evidence child collision"));
    }
    Ok(())
}

async fn ensure_current(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    event_id: &FactEventId,
    updated_at: i64,
) -> Result<()> {
    if row_exists(
        conn,
        "SELECT 1 FROM memory_v2_current_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await?
    {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO memory_v2_current_facts(
            fact_id, owner_kind, project_id, payload_access, trust_score,
            active_assertion_id, last_event_id, updated_at
         ) VALUES(?1, ?2, ?3, 'unavailable', NULL, NULL, ?4, ?5)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            event_id.as_str(),
            updated_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn update_current(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_access: Option<(&FactAssertionId, PayloadAccessState)>,
    trust: Option<f64>,
    event_id: &FactEventId,
    updated_at: i64,
) -> Result<()> {
    let (assertion_id, access) = assertion_access.map_or((None, None), |(id, access)| {
        (Some(id.as_str()), Some(payload_access_label(access)))
    });
    conn.execute(
        "UPDATE memory_v2_current_facts SET
            payload_access = COALESCE(?1, payload_access),
            trust_score = COALESCE(?2, trust_score),
            active_assertion_id = COALESCE(?3, active_assertion_id),
            last_event_id = ?4,
            updated_at = MAX(updated_at, ?5)
         WHERE fact_id = ?6 AND owner_kind = ?7 AND project_id = ?8",
        params![
            access,
            trust,
            assertion_id,
            event_id.as_str(),
            updated_at,
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

/// Merges legacy usage counters into the canonical projection with
/// take-the-maximum semantics: counters only grow, so replaying a crashed
/// backfill batch is idempotent and live retrievals or feedback recorded
/// mid-cutover are never rolled back. The legacy `last_*` recency timestamps
/// are deliberately not carried: a migrated fact's canonical `created_at` is
/// its migration time, and `CompatibilityFactTelemetryV1` rejects recency
/// timestamps earlier than creation, so historical values can never validate.
async fn merge_legacy_fact_telemetry(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    telemetry: &LegacyFactTelemetry,
) -> Result<()> {
    conn.execute(
        "UPDATE memory_v2_current_facts SET
            retrieval_count = MAX(retrieval_count, ?1),
            access_count = MAX(access_count, ?2),
            helpful_count = MAX(helpful_count, ?3),
            unhelpful_count = MAX(unhelpful_count, ?4)
         WHERE fact_id = ?5 AND owner_kind = ?6 AND project_id = ?7",
        params![
            telemetry.retrieval_count.max(0),
            telemetry.access_count.max(0),
            telemetry.helpful_count.max(0),
            telemetry.unhelpful_count.max(0),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn quarantine_fact(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    legacy_fact_id: i64,
    reason: &'static str,
    recorded_at: i64,
) -> Result<()> {
    insert_quarantine(
        conn,
        owner_key,
        source_store_id,
        "memory_facts",
        legacy_fact_id,
        reason,
        recorded_at,
    )
    .await?;
    purge_payload_rows(conn, owner_key, fact_id).await?;
    let previous = current_fact_state(conn, owner_key, fact_id).await?.access;
    let event_id =
        if previous != PayloadAccessState::Deleted && previous != PayloadAccessState::Quarantined {
            let event = FactLineageEventV1::new(
                fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::PayloadAccessChanged {
                    previous,
                    current: PayloadAccessState::Quarantined,
                },
                UtcMicros(recorded_at),
                None,
            )
            .map_err(|_| db_message(OPERATION, "typed quarantine event construction failed"))?;
            insert_event(conn, owner_key, &event, recorded_at).await?;
            Some(event.event_id().clone())
        } else {
            None
        };
    purge_legacy_fact(conn, legacy_fact_id).await?;
    if let Some(event_id) = event_id {
        conn.execute(
            "UPDATE memory_v2_current_facts SET
                payload_access = 'quarantined', active_assertion_id = NULL,
                last_event_id = ?1, updated_at = MAX(updated_at, ?2)
             WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
            params![
                event_id.as_str(),
                recorded_at,
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

async fn purge_memory_v2_fact_inner(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: Option<&FactEventId>,
    occurred_at: UtcMicros,
) -> Result<bool> {
    let legacy_fact_id = optional_i64(
        conn,
        "SELECT legacy_fact_id FROM memory_v2_legacy_map
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
           AND source_store_id = ?4",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await?;
    let fact_exists = row_exists(
        conn,
        "SELECT 1 FROM memory_v2_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await?;
    if !fact_exists {
        return Ok(false);
    }
    if legacy_fact_id.is_none()
        && row_exists(
            conn,
            "SELECT 1 FROM memory_v2_legacy_map
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await?
    {
        return Ok(false);
    }
    let current = current_fact_state(conn, owner_key, fact_id).await?;
    if expected_last_event_id.is_some_and(|expected| expected != &current.last_event_id) {
        return Err(db_message(
            "memory_v2_purge",
            "fact lineage changed before payload purge",
        ));
    }
    if current.access == PayloadAccessState::Deleted {
        return Ok(false);
    }
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: current.access,
            current: PayloadAccessState::Deleted,
        },
        occurred_at,
        None,
    )
    .map_err(|_| {
        db_message(
            "memory_v2_purge",
            "typed deletion event construction failed",
        )
    })?;
    insert_event(conn, owner_key, &event, occurred_at.0).await?;
    purge_payload_rows(conn, owner_key, fact_id).await?;
    if let Some(legacy_fact_id) = legacy_fact_id {
        purge_legacy_fact(conn, legacy_fact_id).await?;
    }
    conn.execute(
        "UPDATE memory_v2_current_facts SET
            payload_access = 'deleted', active_assertion_id = NULL,
            last_event_id = ?1, updated_at = MAX(updated_at, ?2)
         WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
        params![
            event.event_id().as_str(),
            occurred_at.0,
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    Ok(true)
}

async fn purge_payload_rows(conn: &Connection, owner: &OwnerKey, fact_id: &FactId) -> Result<()> {
    // Backfill quarantine reaches this helper without passing through the
    // public purge entrypoint, so set the deletion policy at every destructive
    // payload path.
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "UPDATE memory_v2_feedback_history
         SET source = NULL, note = NULL,
             details_availability = CASE
                 WHEN details_availability = 'available' THEN 'legacy_redacted'
                 ELSE details_availability
             END
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "DELETE FROM memory_v2_assertion_vectors
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "DELETE FROM memory_v2_assertion_payloads
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    Ok(())
}

async fn purge_legacy_fact(conn: &Connection, legacy_fact_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_bank_dirty(bank_name, updated_at)
         SELECT bank_name, ?1 FROM memory_banks
         WHERE 1
         ON CONFLICT(bank_name) DO UPDATE SET updated_at = excluded.updated_at",
        params![current_timestamp()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute("DELETE FROM memory_banks", ())
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let mut rows = conn
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let mut entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?
    {
        entity_ids.push(
            row.get::<i64>(0)
                .map_err(|error| db_error("memory_v2_purge", error))?,
        );
    }
    conn.execute(
        "DELETE FROM memory_facts WHERE fact_id = ?1",
        params![legacy_fact_id],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    for entity_id in entity_ids {
        conn.execute(
            "DELETE FROM memory_entities
             WHERE entity_id = ?1
               AND NOT EXISTS(
                   SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
               )",
            params![entity_id],
        )
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    }
    Ok(())
}

struct SanitizedLegacyMirror<'a> {
    legacy_fact_id: i64,
    content: &'a str,
    category: &'a str,
    tags: &'a [String],
    metadata: &'a Value,
    entities: &'a [String],
    source: &'a str,
    invalidate_vector: bool,
}

async fn mirror_sanitized_legacy(
    conn: &Connection,
    mirror: SanitizedLegacyMirror<'_>,
) -> Result<()> {
    let SanitizedLegacyMirror {
        legacy_fact_id,
        content,
        category,
        tags,
        metadata,
        entities,
        source,
        invalidate_vector,
    } = mirror;
    conn.execute(
        "UPDATE memory_facts SET
            content = ?1, category = ?2, tags = ?3, metadata = ?4, source = ?5,
            hrr_vector = CASE WHEN ?6 THEN NULL ELSE hrr_vector END
         WHERE fact_id = ?7",
        params![
            content,
            category,
            json_text(tags)?,
            json_text(metadata)?,
            source,
            invalidate_vector,
            legacy_fact_id
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    rewrite_legacy_entity_links(conn, legacy_fact_id, entities).await?;
    if invalidate_vector {
        conn.execute(
            "INSERT INTO memory_bank_dirty(bank_name, updated_at)
             SELECT bank_name, ?1 FROM memory_banks
             WHERE 1
             ON CONFLICT(bank_name) DO UPDATE SET updated_at = excluded.updated_at",
            params![current_timestamp()],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
        conn.execute("DELETE FROM memory_banks", ())
            .await
            .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

async fn rewrite_legacy_entity_links(
    conn: &Connection,
    legacy_fact_id: i64,
    entities: &[String],
) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut old_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        old_ids.push(
            row.get::<i64>(0)
                .map_err(|error| db_error(OPERATION, error))?,
        );
    }
    conn.execute(
        "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
        params![legacy_fact_id],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    let mut seen = BTreeSet::new();
    for entity in entities {
        let name = crate::memory::entities::normalize_entity(entity);
        let normalized = name.to_ascii_lowercase();
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            continue;
        }
        let entity_id = if let Some(id) = optional_i64(
            conn,
            "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
            params![normalized.as_str()],
        )
        .await?
        {
            id
        } else {
            conn.execute(
                "INSERT INTO memory_entities(
                    name, normalized_name, entity_type, aliases, created_at, updated_at
                 ) VALUES(?1, ?2, 'unknown', '[]', ?3, ?3)",
                params![name.as_str(), normalized.as_str(), current_timestamp()],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
            optional_i64(
                conn,
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![normalized.as_str()],
            )
            .await?
            .ok_or_else(|| db_message(OPERATION, "sanitized entity insert was not visible"))?
        };
        conn.execute(
            "INSERT INTO memory_fact_entities(fact_id, entity_id) VALUES(?1, ?2)",
            params![legacy_fact_id, entity_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    for entity_id in old_ids {
        conn.execute(
            "DELETE FROM memory_entities
             WHERE entity_id = ?1
               AND NOT EXISTS(
                   SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
               )",
            params![entity_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

async fn load_feedback_history_repair_progress(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
) -> Result<Option<FeedbackHistoryRepairProgress>> {
    let mut rows = conn
        .query(
            "SELECT owner_json, feedback_frontier, feedback_cursor, phase, started_at
             FROM memory_v2_feedback_history_repair_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    else {
        return Ok(None);
    };
    let owner_json = row
        .get::<String>(0)
        .map_err(|error| db_error(OPERATION, error))?;
    if owner_json != owner.json {
        return Err(db_message(
            OPERATION,
            "feedback history repair owner identity does not match progress",
        ));
    }
    let progress = FeedbackHistoryRepairProgress {
        feedback_frontier: row.get(1).map_err(|error| db_error(OPERATION, error))?,
        feedback_cursor: row.get(2).map_err(|error| db_error(OPERATION, error))?,
        phase: row.get(3).map_err(|error| db_error(OPERATION, error))?,
        started_at: row.get(4).map_err(|error| db_error(OPERATION, error))?,
    };
    if progress.feedback_cursor > progress.feedback_frontier {
        return Err(db_message(
            OPERATION,
            "feedback history repair cursor exceeds captured frontier",
        ));
    }
    Ok(Some(progress))
}

async fn advance_feedback_history_repair(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    cursor: i64,
) -> Result<()> {
    let updated_at = now_micros()?;
    let changed = conn
        .execute(
            "UPDATE memory_v2_feedback_history_repair_progress
             SET feedback_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5
               AND phase = 'pending' AND feedback_cursor <= ?1",
            params![
                cursor,
                updated_at,
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if changed != 1 {
        return Err(db_message(
            OPERATION,
            "feedback history repair progress was not advanceable",
        ));
    }
    Ok(())
}

async fn complete_feedback_history_repair(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    cursor: i64,
) -> Result<()> {
    let completed_at = now_micros()?;
    let changed = conn
        .execute(
            "UPDATE memory_v2_feedback_history_repair_progress
             SET feedback_cursor = ?1, phase = 'complete',
                 updated_at = ?2, completed_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5
               AND phase = 'pending' AND feedback_frontier = ?1
               AND feedback_cursor <= ?1",
            params![
                cursor,
                completed_at,
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if changed != 1 {
        return Err(db_message(
            OPERATION,
            "feedback history repair progress was not completable",
        ));
    }
    Ok(())
}

async fn load_or_create_progress(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    frontiers: CapturedMemoryV2Frontiers,
) -> Result<Progress> {
    let mut rows = conn
        .query(
            "SELECT phase, feedback_frontier, oplog_frontier, fact_frontier,
                    feedback_cursor, oplog_cursor, fact_cursor, started_at
             FROM memory_v2_backfill_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let progress = Progress {
            phase: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            feedback_frontier: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            oplog_frontier: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            fact_frontier: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            feedback_cursor: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            oplog_cursor: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            fact_cursor: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            started_at: row.get(7).map_err(|error| db_error(OPERATION, error))?,
        };
        if (
            progress.feedback_frontier,
            progress.oplog_frontier,
            progress.fact_frontier,
        ) != (frontiers.feedback, frontiers.oplog, frontiers.facts)
        {
            return Err(db_message(
                OPERATION,
                "captured backfill frontier changed across retry",
            ));
        }
        return Ok(progress);
    }
    let started_at = now_micros()?;
    conn.execute(
        "INSERT INTO memory_v2_backfill_progress(
            owner_kind, project_id, owner_json, source_store_id, phase,
            feedback_frontier, oplog_frontier, fact_frontier, started_at, updated_at
         ) VALUES(?1, ?2, ?3, ?4, 'feedback', ?5, ?6, ?7, ?8, ?8)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            source_store_id.as_str(),
            frontiers.feedback,
            frontiers.oplog,
            frontiers.facts,
            started_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(Progress {
        phase: "feedback".to_owned(),
        feedback_frontier: frontiers.feedback,
        oplog_frontier: frontiers.oplog,
        fact_frontier: frontiers.facts,
        feedback_cursor: 0,
        oplog_cursor: 0,
        fact_cursor: 0,
        started_at,
    })
}

async fn update_phase(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    phase: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE memory_v2_backfill_progress SET phase = ?1, updated_at = ?2
         WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5",
        params![
            phase,
            now_micros()?,
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn update_cursor(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    column: &str,
    cursor: i64,
) -> Result<()> {
    let sql = match column {
        "feedback_cursor" => {
            "UPDATE memory_v2_backfill_progress
             SET feedback_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5"
        }
        "oplog_cursor" => {
            "UPDATE memory_v2_backfill_progress
             SET oplog_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5"
        }
        "fact_cursor" => {
            "UPDATE memory_v2_backfill_progress
             SET fact_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5"
        }
        _ => return Err(db_message(OPERATION, "invalid backfill cursor column")),
    };
    conn.execute(
        sql,
        params![
            cursor,
            now_micros()?,
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn insert_quarantine(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    source_table: &'static str,
    source_row_id: i64,
    reason_code: &'static str,
    recorded_at: i64,
) -> Result<()> {
    if let Some(existing) = optional_string(
        conn,
        "SELECT reason_code FROM memory_v2_legacy_quarantine
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND source_table = ?4 AND source_row_id = ?5",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            source_table,
            source_row_id
        ],
    )
    .await?
    {
        return canonical_replay(existing, reason_code, "legacy quarantine record");
    }
    conn.execute(
        "INSERT INTO memory_v2_legacy_quarantine(
            owner_kind, project_id, source_store_id, source_table,
            source_row_id, reason_code, recorded_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            source_table,
            source_row_id,
            reason_code,
            recorded_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn current_fact_state(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> Result<CurrentFactState> {
    let mut rows = conn
        .query(
            "SELECT current.payload_access, current.last_event_id,
                current.active_assertion_id, assertion.kind_json,
                assertion.payload_reference_json
         FROM memory_v2_current_facts current
         LEFT JOIN memory_v2_assertions assertion
           ON assertion.assertion_id = current.active_assertion_id
          AND assertion.fact_id = current.fact_id
          AND assertion.owner_kind = current.owner_kind
          AND assertion.project_id = current.project_id
         WHERE current.fact_id = ?1
           AND current.owner_kind = ?2 AND current.project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "current fact projection is missing"))?;
    let access = row
        .get::<String>(0)
        .map_err(|error| db_error(OPERATION, error))?;
    let event_id = FactEventId::new(
        row.get::<String>(1)
            .map_err(|error| db_error(OPERATION, error))?,
    )
    .map_err(|_| db_message(OPERATION, "stored last event identity is invalid"))?;
    let active_assertion_id = row
        .get::<Option<String>>(2)
        .map_err(|error| db_error(OPERATION, error))?
        .map(FactAssertionId::new)
        .transpose()
        .map_err(|_| db_message(OPERATION, "stored active assertion identity is invalid"))?;
    let active_kind = row
        .get::<Option<String>>(3)
        .map_err(|error| db_error(OPERATION, error))?
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|_| db_message(OPERATION, "stored assertion kind is invalid"))?;
    let active_payload_reference = row
        .get::<Option<String>>(4)
        .map_err(|error| db_error(OPERATION, error))?
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|_| db_message(OPERATION, "stored payload reference is invalid"))?;
    Ok(CurrentFactState {
        access: parse_payload_access(&access)?,
        last_event_id: event_id,
        active_assertion_id,
        active_kind,
        active_payload_reference,
    })
}

async fn load_legacy_entities(conn: &Connection, legacy_fact_id: i64) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT e.name FROM memory_entities e
             JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
             WHERE fe.fact_id = ?1 ORDER BY e.name",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut entities = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        entities.push(row.get(0).map_err(|error| db_error(OPERATION, error))?);
    }
    Ok(entities)
}

fn owner_key(owner: &FactOwnerV1) -> Result<OwnerKey> {
    owner
        .validate()
        .map_err(|_| db_message(OPERATION, "fact owner is invalid"))?;
    let (kind, project_id) = match owner {
        FactOwnerV1::Profile => ("profile", String::new()),
        FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
    };
    Ok(OwnerKey {
        kind,
        project_id,
        json: json_text(owner)?,
    })
}

fn validate_scope(owner: &FactOwnerV1, source_store_id: &SourceStoreId) -> Result<()> {
    owner
        .validate()
        .map_err(|_| db_message(OPERATION, "fact owner is invalid"))?;
    source_store_id
        .validate()
        .map_err(|_| db_message(OPERATION, "source store identity is invalid"))?;
    Ok(())
}

fn validate_v1_compatibility_source(source_store_id: &SourceStoreId) -> Result<()> {
    if source_store_id.as_str() == V1_COMPATIBILITY_SOURCE_STORE {
        Ok(())
    } else {
        Err(db_message(
            OPERATION,
            "V1 compatibility mappings require the fixed legacy-memory-v1 source store",
        ))
    }
}

fn validate_frontiers(frontiers: CapturedMemoryV2Frontiers) -> Result<()> {
    if frontiers.feedback < 0 || frontiers.oplog < 0 || frontiers.facts < 0 {
        Err(db_message(
            OPERATION,
            "backfill frontier cannot be negative",
        ))
    } else {
        Ok(())
    }
}

fn parse_category(value: &str) -> std::result::Result<tracedecay_domain::FactCategoryV1, ()> {
    use tracedecay_domain::FactCategoryV1;
    match value {
        "general" => Ok(FactCategoryV1::General),
        "user_pref" => Ok(FactCategoryV1::UserPref),
        "project" => Ok(FactCategoryV1::Project),
        "tool" => Ok(FactCategoryV1::Tool),
        "decision" => Ok(FactCategoryV1::Decision),
        "code_area" => Ok(FactCategoryV1::CodeArea),
        _ => Err(()),
    }
}

fn category_label(category: tracedecay_domain::FactCategoryV1) -> &'static str {
    use tracedecay_domain::FactCategoryV1;
    match category {
        FactCategoryV1::General => "general",
        FactCategoryV1::UserPref => "user_pref",
        FactCategoryV1::Project => "project",
        FactCategoryV1::Tool => "tool",
        FactCategoryV1::Decision => "decision",
        FactCategoryV1::CodeArea => "code_area",
    }
}

fn payload_access_label(state: PayloadAccessState) -> &'static str {
    match state {
        PayloadAccessState::Eligible => "eligible",
        PayloadAccessState::Redacted => "redacted",
        PayloadAccessState::Quarantined => "quarantined",
        PayloadAccessState::RetentionExpired => "retention_expired",
        PayloadAccessState::Deleted => "deleted",
        PayloadAccessState::Unavailable => "unavailable",
        PayloadAccessState::Ambiguous => "ambiguous",
    }
}

fn parse_payload_access(value: &str) -> Result<PayloadAccessState> {
    match value {
        "eligible" => Ok(PayloadAccessState::Eligible),
        "redacted" => Ok(PayloadAccessState::Redacted),
        "quarantined" => Ok(PayloadAccessState::Quarantined),
        "retention_expired" => Ok(PayloadAccessState::RetentionExpired),
        "deleted" => Ok(PayloadAccessState::Deleted),
        "unavailable" => Ok(PayloadAccessState::Unavailable),
        "ambiguous" => Ok(PayloadAccessState::Ambiguous),
        _ => Err(db_message(
            OPERATION,
            "stored payload access state is invalid",
        )),
    }
}

fn value_strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn seconds_to_micros(seconds: i64) -> Option<UtcMicros> {
    seconds.checked_mul(1_000_000).map(UtcMicros)
}

fn sanitize_legacy_feedback_details(
    source: Option<&str>,
    note: Option<&str>,
) -> (
    Option<String>,
    Option<String>,
    &'static str,
    Option<&'static str>,
) {
    let Some(source) = source else {
        return (None, None, "unknown", Some("feedback_details_unknown"));
    };
    let Some(source) = sanitize_provider_metadata_text(source) else {
        return (
            None,
            None,
            "legacy_redacted",
            Some("feedback_details_redacted"),
        );
    };
    let note = match note {
        Some(note) => match sanitize_provider_metadata_text(note) {
            Some(note) => Some(note),
            None => {
                return (
                    None,
                    None,
                    "legacy_redacted",
                    Some("feedback_details_redacted"),
                );
            }
        },
        None => None,
    };
    (Some(source), note, "available", None)
}

fn now_micros() -> Result<i64> {
    current_timestamp()
        .checked_mul(1_000_000)
        .ok_or_else(|| db_message(OPERATION, "current timestamp is outside supported range"))
}

fn json_text(value: &(impl Serialize + ?Sized)) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| db_message(OPERATION, "canonical JSON encoding failed"))
}

fn canonical_replay(existing: String, candidate: &str, record: &str) -> Result<()> {
    if existing == candidate {
        Ok(())
    } else {
        Err(db_message(
            OPERATION,
            format!("{record} identity collision"),
        ))
    }
}

/// A cutover command's identity is its receipt id, owner/source, and drained
/// frontier. Its completion timestamp is generated by the first successful
/// finalization and must not make a retry collide with that completed receipt.
fn canonical_cutover_replay(existing: String, candidate: &str) -> Result<()> {
    canonical_replay(
        cutover_replay_identity(&existing)?,
        &cutover_replay_identity(candidate)?,
        "cutover receipt",
    )
}

fn cutover_replay_identity(receipt_json: &str) -> Result<String> {
    let mut receipt: Value = serde_json::from_str(receipt_json)
        .map_err(|_| db_message(OPERATION, "stored cutover receipt is invalid JSON"))?;
    let object = receipt
        .as_object_mut()
        .ok_or_else(|| db_message(OPERATION, "stored cutover receipt is not an object"))?;
    if object.remove("dual_write_activated_at").is_none() {
        return Err(db_message(
            OPERATION,
            "stored cutover receipt lacks its completion timestamp",
        ));
    }
    json_text(&receipt)
}

async fn begin(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .await
        .map(|_| ())
        .map_err(|error| db_error(operation, error))
}

async fn finish_transaction<T>(conn: &Connection, result: Result<T>, operation: &str) -> Result<T> {
    match result {
        Ok(value) => match conn.execute_batch("COMMIT").await {
            Ok(_) => Ok(value),
            Err(commit_error) => {
                let rollback = conn.execute_batch("ROLLBACK").await;
                let cleanup = if rollback.is_ok() {
                    "rollback completed"
                } else {
                    "rollback failed; writer connection must be retired"
                };
                Err(db_message(
                    operation,
                    format!("commit failed ({cleanup}): {commit_error}"),
                ))
            }
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

async fn scalar_i64(conn: &Connection, sql: &str) -> Result<i64> {
    scalar_i64_params(conn, sql, ()).await
}

async fn scalar_i64_params(
    conn: &Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> Result<i64> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "scalar query returned no row"))?
        .get(0)
        .map_err(|error| db_error(OPERATION, error))
}

async fn optional_string(
    conn: &Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> Result<Option<String>> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .map(|row| row.get(0).map_err(|error| db_error(OPERATION, error)))
        .transpose()
}

async fn optional_i64(
    conn: &Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> Result<Option<i64>> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .map(|row| row.get(0).map_err(|error| db_error(OPERATION, error)))
        .transpose()
}

async fn row_exists(
    conn: &Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> Result<bool> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .is_some())
}

fn db_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: storage operation failed: {error}"),
        operation: operation.to_owned(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use libsql::Builder;
    use tempfile::TempDir;
    use tracedecay_domain::{FactIdentityMaterialV1, FactIdentitySourceV1};

    use super::*;

    async fn database() -> (Connection, libsql::Database, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-v2.db");
        let db = Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
            .await
            .unwrap();
        crate::db::migrations::create_schema(&conn).await.unwrap();
        (conn, db, dir)
    }

    async fn pre_v22_database() -> (Connection, libsql::Database, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-v2-pre-v22.db");
        let db = Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
            .await
            .unwrap();
        create_schema(&conn, "memory_v2_pre_v22_test")
            .await
            .unwrap();
        upgrade_v20_schema(&conn, "memory_v2_pre_v22_test")
            .await
            .unwrap();
        upgrade_v21_schema(&conn, "memory_v2_pre_v22_test")
            .await
            .unwrap();
        (conn, db, dir)
    }

    fn owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: tracedecay_domain::ProjectId::new("project.memory-v2-test").unwrap(),
        }
    }

    fn source_store_id() -> SourceStoreId {
        SourceStoreId::new(V1_COMPATIBILITY_SOURCE_STORE).unwrap()
    }

    async fn run_to_frontier(
        conn: &Connection,
        owner: &FactOwnerV1,
        source: &SourceStoreId,
        frontiers: CapturedMemoryV2Frontiers,
        batch_size: i64,
    ) {
        for _ in 0..32 {
            if backfill_memory_v2_batch(conn, owner, source, frontiers, batch_size)
                .await
                .unwrap()
                == MemoryV2BackfillBatchOutcome::AwaitingCutover
            {
                return;
            }
        }
        panic!("backfill did not reach captured frontier");
    }

    async fn scalar(conn: &Connection, sql: &str) -> i64 {
        scalar_i64(conn, sql).await.unwrap()
    }

    #[tokio::test]
    async fn schema_install_does_not_start_unowned_backfill() {
        let (conn, _db, _dir) = database().await;
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_backfill_progress").await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM retrieval_anchors").await,
            0
        );
        assert!(
            !row_exists(
                &conn,
                "SELECT 1 FROM sqlite_master WHERE name = 'memory_v2_retrieval_anchors'",
                (),
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn v20_and_v21_installers_do_not_leak_v22_or_v23_schema() {
        let (conn, _db, _dir) = pre_v22_database().await;
        assert!(
            !table_exists(&conn, "memory_v2_compatibility_operation_receipts")
                .await
                .unwrap()
        );
        assert!(
            !table_exists(&conn, "memory_v2_feedback_history_repair_progress")
                .await
                .unwrap()
        );
        assert!(
            !table_exists(&conn, "memory_v2_fact_relations")
                .await
                .unwrap()
        );
        assert!(
            !table_exists(&conn, "memory_v2_compatibility_banks")
                .await
                .unwrap()
        );
        assert!(
            !table_exists(&conn, "memory_v2_compatibility_bank_dirty")
                .await
                .unwrap()
        );
        assert!(!proposal_schema_is_v22(&conn).await.unwrap());

        install_v22_fresh_schema(&conn, "memory_v2_v22_fresh_test")
            .await
            .unwrap();
        assert!(
            table_exists(&conn, "memory_v2_compatibility_operation_receipts")
                .await
                .unwrap()
        );
        assert!(
            table_exists(&conn, "memory_v2_feedback_history_repair_progress")
                .await
                .unwrap()
        );
        assert!(
            table_exists(&conn, "memory_v2_fact_relations")
                .await
                .unwrap()
        );
        assert!(
            !table_exists(&conn, "memory_v2_compatibility_banks")
                .await
                .unwrap()
        );
        assert!(
            !table_exists(&conn, "memory_v2_compatibility_bank_dirty")
                .await
                .unwrap()
        );
        assert!(
            !table_has_column(
                &conn,
                "memory_v2_fact_relations",
                "provenance_json",
                "memory_v2_v22_fresh_test",
            )
            .await
            .unwrap()
        );
        assert!(proposal_schema_is_v22(&conn).await.unwrap());

        install_v23_fresh_schema(&conn, "memory_v2_v23_fresh_test")
            .await
            .unwrap();
        assert!(
            table_exists(&conn, "memory_v2_compatibility_banks")
                .await
                .unwrap()
        );
        assert!(
            table_exists(&conn, "memory_v2_compatibility_bank_dirty")
                .await
                .unwrap()
        );
        assert!(
            table_has_column(
                &conn,
                "memory_v2_fact_relations",
                "provenance_json",
                "memory_v2_v23_fresh_test",
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn v23_rebuilds_v22_fact_relations_without_losing_rows() {
        let (conn, _db, _dir) = pre_v22_database().await;
        install_v22_fresh_schema(&conn, "memory_v2_v23_relation_upgrade_test")
            .await
            .unwrap();
        let owner = owner_key(&owner()).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES
                ('v23.relation.source', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
                ('v23.relation.target', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
                ('v23.relation.evidence', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1);
             INSERT INTO memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, evidence_fact_ids_json, occurred_at, updated_at
             ) VALUES(
                '{kind}', '{project_id}', 'v23.relation.source', 'v23.relation.target',
                'supports', 0.8, 'fixture', '[\"v23.relation.evidence\"]', 1, 1
             );",
            kind = owner.kind,
            project_id = owner.project_id,
            owner_json = owner.json,
        ))
        .await
        .unwrap();

        conn.execute("PRAGMA user_version = 22", ()).await.unwrap();
        assert!(
            super::super::migrations::migrate(&conn)
                .await
                .expect("V22 relation fixture must migrate to V23")
        );
        assert_eq!(
            optional_i64(&conn, "PRAGMA user_version", ())
                .await
                .unwrap(),
            Some(23)
        );
        assert!(
            table_exists(&conn, "memory_v2_compatibility_banks")
                .await
                .unwrap()
        );
        assert!(
            table_exists(&conn, "memory_v2_compatibility_bank_dirty")
                .await
                .unwrap()
        );
        assert!(
            table_has_column(
                &conn,
                "memory_v2_fact_relations",
                "provenance_json",
                "memory_v2_v23_relation_upgrade_test",
            )
            .await
            .unwrap()
        );
        assert_eq!(
            optional_string(
                &conn,
                "SELECT provenance_json FROM memory_v2_fact_relations
                 WHERE source_fact_id = 'v23.relation.source'
                   AND target_fact_id = 'v23.relation.target' AND relation = 'supports'",
                (),
            )
            .await
            .unwrap(),
            Some("{}".to_owned())
        );
        conn.execute(
            "INSERT INTO memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, provenance_json, evidence_fact_ids_json,
                occurred_at, updated_at
             ) VALUES(?1, ?2, 'v23.relation.source', 'v23.relation.target',
                       'contradicts', 0.8, 'fixture', '{}',
                       '[\"v23.relation.evidence\"]', 2, 2)",
            params![owner.kind, owner.project_id.as_str()],
        )
        .await
        .unwrap();
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_fact_relations").await,
            2
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
            0
        );
    }

    #[tokio::test]
    async fn v20_v21_feedback_backfill_does_not_require_v22_history_tables() {
        let (conn, _db, _dir) = pre_v22_database().await;
        conn.execute_batch(
            "CREATE TABLE memory_feedback_events (
                event_id INTEGER PRIMARY KEY,
                fact_id INTEGER NOT NULL,
                action TEXT NOT NULL,
                old_trust REAL NOT NULL,
                new_trust REAL NOT NULL,
                created_at INTEGER NOT NULL,
                source TEXT,
                note TEXT
             );
             INSERT INTO memory_feedback_events(
                event_id, fact_id, action, old_trust, new_trust, created_at, source, note
             ) VALUES(1, 7, 'helpful', 0.5, 0.6, 10, 'mcp', 'legacy note');",
        )
        .await
        .unwrap();
        let owner = owner();
        let source = source_store_id();
        let owner_key = owner_key(&owner).unwrap();
        conn.execute(
            "INSERT INTO memory_v2_backfill_progress(
                owner_kind, project_id, owner_json, source_store_id, phase,
                feedback_frontier, oplog_frontier, fact_frontier, started_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, 'feedback', 1, 0, 0, 1, 1)",
            params![
                owner_key.kind,
                owner_key.project_id.as_str(),
                owner_key.json.as_str(),
                source.as_str()
            ],
        )
        .await
        .unwrap();
        let progress = Progress {
            phase: "feedback".to_owned(),
            feedback_frontier: 1,
            oplog_frontier: 0,
            fact_frontier: 0,
            feedback_cursor: 0,
            oplog_cursor: 0,
            fact_cursor: 0,
            started_at: 1,
        };

        assert_eq!(
            backfill_feedback_batch(&conn, &owner, &owner_key, &source, &progress, 1)
                .await
                .unwrap(),
            MemoryV2BackfillBatchOutcome::Advanced { processed: 1 }
        );
        assert!(
            !table_exists(&conn, "memory_v2_legacy_feedback_event_map")
                .await
                .unwrap()
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_lineage_events").await,
            2,
            "V20/V21 retain their original typed import and trust lineage"
        );
    }

    #[tokio::test]
    async fn v22_feedback_history_repair_is_bounded_idempotent_and_redactable() {
        let (conn, _db, _dir) = pre_v22_database().await;
        conn.execute_batch(
            "CREATE TABLE memory_facts (
                fact_id INTEGER PRIMARY KEY,
                content TEXT NOT NULL
             );
             CREATE TABLE memory_feedback_events (
                event_id INTEGER PRIMARY KEY,
                fact_id INTEGER NOT NULL,
                action TEXT NOT NULL,
                old_trust REAL NOT NULL,
                new_trust REAL NOT NULL,
                created_at INTEGER NOT NULL,
                source TEXT,
                note TEXT
             );
             INSERT INTO memory_facts(fact_id, content) VALUES(7, 'legacy feedback fact');
             INSERT INTO memory_feedback_events(
                event_id, fact_id, action, old_trust, new_trust, created_at, source, note
             ) VALUES
                (9, 7, 'helpful', 0.5, 0.55, 10, 'mcp', 'safe note'),
                (10, 7, 'unhelpful', 0.55, 0.45, 11, NULL, NULL),
                (11, 7, 'helpful', 0.5, 0.55, 10, 'mcp', 'duplicate');",
        )
        .await
        .unwrap();

        let owner = owner();
        let source = source_store_id();
        let owner_key = owner_key(&owner).unwrap();
        let material = FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Legacy {
                source_store_id: source.clone(),
                legacy_fact_id: 7,
            },
        )
        .unwrap();
        let fact_id = FactId::derive(&material).unwrap();
        insert_fact_identity(
            &conn,
            &owner_key,
            &fact_id,
            &json_text(&material).unwrap(),
            1,
        )
        .await
        .unwrap();
        let mapping = LegacyFactMappingV1::new(
            owner.clone(),
            source.clone(),
            7,
            fact_id.clone(),
            LegacyHistoryCoverageV1::Unknown,
            UtcMicros(1),
        )
        .unwrap();
        insert_mapping(&conn, &owner_key, &mapping).await.unwrap();
        let imported = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::LegacyImported { mapping },
            UtcMicros(1),
            None,
        )
        .unwrap();
        insert_event(&conn, &owner_key, &imported, 1).await.unwrap();
        ensure_current(&conn, &owner_key, &fact_id, imported.event_id(), 1)
            .await
            .unwrap();
        let first = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: Confidence::new(0.5).unwrap(),
                current: Confidence::new(0.55).unwrap(),
                evidence_ids: Vec::new(),
            },
            UtcMicros(10_000_000),
            None,
        )
        .unwrap();
        insert_event(&conn, &owner_key, &first, 1).await.unwrap();
        update_current(
            &conn,
            &owner_key,
            &fact_id,
            None,
            Some(0.55),
            first.event_id(),
            10_000_000,
        )
        .await
        .unwrap();
        let second = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: Confidence::new(0.55).unwrap(),
                current: Confidence::new(0.45).unwrap(),
                evidence_ids: Vec::new(),
            },
            UtcMicros(11_000_000),
            None,
        )
        .unwrap();
        insert_event(&conn, &owner_key, &second, 1).await.unwrap();
        update_current(
            &conn,
            &owner_key,
            &fact_id,
            None,
            Some(0.45),
            second.event_id(),
            11_000_000,
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_backfill_progress(
                owner_kind, project_id, owner_json, source_store_id, phase,
                feedback_frontier, oplog_frontier, fact_frontier,
                feedback_cursor, oplog_cursor, fact_cursor,
                started_at, updated_at, cutover_completed_at, cutover_receipt_json
             ) VALUES(?1, ?2, ?3, ?4, 'cutover_complete', 11, 0, 0, 11, 0, 0, 1, 1, 1, '{}')",
            params![
                owner_key.kind,
                owner_key.project_id.as_str(),
                owner_key.json.as_str(),
                source.as_str()
            ],
        )
        .await
        .unwrap();

        upgrade_v22_schema(&conn, "memory_v2_v22_repair_test")
            .await
            .unwrap();
        assert_eq!(
            feedback_history_repair_progress(&conn, &owner, &source)
                .await
                .unwrap(),
            Some(MemoryV2FeedbackHistoryRepairProgress {
                feedback_frontier: 11,
                feedback_cursor: 0,
                complete: false,
            })
        );
        let transaction = conn.transaction().await.unwrap();
        assert_eq!(
            repair_memory_v2_feedback_history_batch_in_transaction(
                &transaction,
                &owner,
                &source,
                1,
            )
            .await
            .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced { processed: 1 }
        );
        transaction.rollback().await.unwrap();
        assert_eq!(
            feedback_history_repair_progress(&conn, &owner, &source)
                .await
                .unwrap(),
            Some(MemoryV2FeedbackHistoryRepairProgress {
                feedback_frontier: 11,
                feedback_cursor: 0,
                complete: false,
            }),
            "an enclosing receipt transaction must roll back the repair cursor too"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_legacy_feedback_event_map",
            )
            .await,
            0,
            "an enclosing receipt transaction must roll back repair projections"
        );
        assert_eq!(
            repair_memory_v2_feedback_history_batch(&conn, &owner, &source, 1)
                .await
                .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced { processed: 1 }
        );
        assert_eq!(
            repair_memory_v2_feedback_history_batch(&conn, &owner, &source, 1)
                .await
                .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced { processed: 1 }
        );
        assert_eq!(
            repair_memory_v2_feedback_history_batch(&conn, &owner, &source, 1)
                .await
                .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 1 }
        );
        assert_eq!(
            repair_memory_v2_feedback_history_batch(&conn, &owner, &source, 1)
                .await
                .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 0 }
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_legacy_feedback_event_map"
            )
            .await,
            2
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_feedback_history").await,
            2
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_feedback_history
                 WHERE details_availability = 'available' AND source = 'mcp' AND note = 'safe note'",
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_legacy_quarantine
                 WHERE reason_code = 'feedback_event_duplicate'",
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_feedback_history
                 WHERE details_availability = 'unknown' AND source IS NULL AND note IS NULL",
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_legacy_quarantine
                 WHERE reason_code = 'feedback_details_unknown'",
            )
            .await,
            1
        );
        purge_payload_rows(&conn, &owner_key, &fact_id)
            .await
            .unwrap();
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_feedback_history
                 WHERE details_availability = 'legacy_redacted'
                   AND source IS NULL AND note IS NULL",
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_feedback_history
                 WHERE details_availability = 'unknown'
                   AND source IS NULL AND note IS NULL",
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn v22_feedback_history_repair_skips_foreign_rows_and_yields_after_a_bounded_slice() {
        let (conn, _db, _dir) = pre_v22_database().await;
        let owner = owner();
        let source = source_store_id();
        let primary_owner_key = owner_key(&owner).unwrap();
        ensure_legacy_identity(&conn, &owner, &primary_owner_key, &source, 1, 1)
            .await
            .unwrap();
        let foreign_owner = FactOwnerV1::Project {
            project_id: tracedecay_domain::ProjectId::new("project.memory-v2-foreign").unwrap(),
        };
        let foreign_owner_key = owner_key(&foreign_owner).unwrap();
        ensure_legacy_identity(&conn, &foreign_owner, &foreign_owner_key, &source, 2, 1)
            .await
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE memory_feedback_events (
                event_id INTEGER PRIMARY KEY,
                fact_id INTEGER NOT NULL,
                action TEXT NOT NULL,
                old_trust REAL NOT NULL,
                new_trust REAL NOT NULL,
                created_at INTEGER NOT NULL,
                source TEXT,
                note TEXT
             );
             WITH RECURSIVE ids(event_id) AS (
                VALUES(1)
                UNION ALL
                SELECT event_id + 1 FROM ids WHERE event_id < 514
             )
             INSERT INTO memory_feedback_events(
                event_id, fact_id, action, old_trust, new_trust, created_at, source, note
             )
             SELECT event_id,
                    CASE WHEN event_id = 1 THEN 2 ELSE 1 END,
                    'unsupported', 0.5, 0.5, event_id, NULL, NULL
             FROM ids;",
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_backfill_progress(
                owner_kind, project_id, owner_json, source_store_id, phase,
                feedback_frontier, oplog_frontier, fact_frontier,
                feedback_cursor, oplog_cursor, fact_cursor,
                started_at, updated_at, cutover_completed_at, cutover_receipt_json
            ) VALUES(?1, ?2, ?3, ?4, 'cutover_complete',
                514, 0, 0, 514, 0, 0, 1, 1, 1, '{}')",
            params![
                primary_owner_key.kind,
                primary_owner_key.project_id.as_str(),
                primary_owner_key.json.as_str(),
                source.as_str()
            ],
        )
        .await
        .unwrap();

        upgrade_v22_schema(&conn, "memory_v2_v22_bounded_repair_test")
            .await
            .unwrap();
        assert!(
            repair_memory_v2_feedback_history_batch(
                &conn,
                &owner,
                &source,
                MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE + 1,
            )
            .await
            .is_err(),
            "repair must reject a slice larger than the fixed V22 bound"
        );
        assert_eq!(
            repair_memory_v2_feedback_history_batch(
                &conn,
                &owner,
                &source,
                MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE,
            )
            .await
            .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced { processed: 512 }
        );
        assert_eq!(
            feedback_history_repair_progress(&conn, &owner, &source)
                .await
                .unwrap(),
            Some(MemoryV2FeedbackHistoryRepairProgress {
                feedback_frontier: 514,
                feedback_cursor: 513,
                complete: false,
            })
        );
        assert_eq!(
            repair_memory_v2_feedback_history_batch(
                &conn,
                &owner,
                &source,
                MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE,
            )
            .await
            .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 1 }
        );
        assert_eq!(
            feedback_history_repair_progress(&conn, &owner, &source)
                .await
                .unwrap(),
            Some(MemoryV2FeedbackHistoryRepairProgress {
                feedback_frontier: 514,
                feedback_cursor: 514,
                complete: true,
            })
        );
        assert_eq!(
            repair_memory_v2_feedback_history_batch(
                &conn,
                &owner,
                &source,
                MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE,
            )
            .await
            .unwrap(),
            MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 0 }
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_legacy_quarantine
                 WHERE source_table = 'memory_feedback_events'
                   AND reason_code = 'unknown_feedback_action'",
            )
            .await,
            513
        );
    }

    #[tokio::test]
    async fn bounded_backfill_resumes_with_fixed_frontiers_and_unknown_history() {
        let (conn, db, _dir) = database().await;
        for id in 1..=3 {
            conn.execute(
                "INSERT INTO memory_facts(
                    fact_id, content, category, tags, trust_score, source,
                    metadata, hrr_vector, created_at, updated_at
                 ) VALUES(?1, ?2, 'project', '[]', 0.5, 'manual', '{}', x'0102', 10, 10)",
                params![id, format!("bounded fact {id}")],
            )
            .await
            .unwrap();
        }
        let owner = owner();
        let source = source_store_id();
        let frontiers = load_or_capture_memory_v2_frontiers(&conn, &owner, &source)
            .await
            .unwrap();
        assert_eq!(
            backfill_memory_v2_batch(&conn, &owner, &source, frontiers, 1)
                .await
                .unwrap(),
            MemoryV2BackfillBatchOutcome::Advanced { processed: 0 }
        );
        drop(conn);
        let restarted = db.connect().unwrap();
        restarted
            .execute_batch("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        run_to_frontier(&restarted, &owner, &source, frontiers, 1).await;
        assert_eq!(
            scalar(&restarted, "SELECT COUNT(*) FROM memory_v2_legacy_map").await,
            3
        );
        let mut rows = restarted
            .query(
                "SELECT mapping_json FROM memory_v2_legacy_map ORDER BY legacy_fact_id",
                (),
            )
            .await
            .unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            let mapping: LegacyFactMappingV1 =
                serde_json::from_str(&row.get::<String>(0).unwrap()).unwrap();
            assert_eq!(mapping.history_coverage(), LegacyHistoryCoverageV1::Unknown);
        }
        let phase = optional_string(
            &restarted,
            "SELECT phase FROM memory_v2_backfill_progress",
            (),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(phase, "awaiting_cutover");
        assert_eq!(
            scalar(
                &restarted,
                "SELECT COUNT(*) FROM memory_v2_lineage_events WHERE json_valid(event_json)"
            )
            .await,
            9
        );
    }

    #[tokio::test]
    async fn cutover_replay_preserves_first_completion_time_but_binds_frontier() {
        let (conn, _db, _dir) = database().await;
        conn.execute(
            "INSERT INTO memory_facts(
                fact_id, content, category, tags, trust_score, source, metadata,
                hrr_vector, created_at, updated_at
             ) VALUES(1, 'cutover replay fact', 'project', '[]', 0.5, 'manual', '{}',
                      x'0102', 10, 10)",
            (),
        )
        .await
        .unwrap();
        let owner = owner();
        let source = source_store_id();
        let frontiers = load_or_capture_memory_v2_frontiers(&conn, &owner, &source)
            .await
            .unwrap();
        run_to_frontier(&conn, &owner, &source, frontiers, 1).await;
        let receipt_id = ProvenanceId::new("memory-v2.cutover-replay".to_owned()).unwrap();
        let first = MemoryV2CutoverReceipt::new(
            receipt_id.clone(),
            owner.clone(),
            source.clone(),
            frontiers,
            UtcMicros(1_000),
        )
        .unwrap();
        assert_eq!(
            finalize_memory_v2_cutover(&conn, &first).await.unwrap(),
            MemoryV2CutoverOutcome::Complete
        );
        let stored_receipt = optional_string(
            &conn,
            "SELECT cutover_receipt_json FROM memory_v2_backfill_progress",
            (),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            scalar(
                &conn,
                "SELECT cutover_completed_at FROM memory_v2_backfill_progress"
            )
            .await,
            1_000
        );

        let replay = MemoryV2CutoverReceipt::new(
            receipt_id.clone(),
            owner.clone(),
            source.clone(),
            frontiers,
            UtcMicros(2_000),
        )
        .unwrap();
        assert_eq!(
            finalize_memory_v2_cutover(&conn, &replay).await.unwrap(),
            MemoryV2CutoverOutcome::Complete,
            "a retry must retain the first durable completion timestamp"
        );
        assert_eq!(
            optional_string(
                &conn,
                "SELECT cutover_receipt_json FROM memory_v2_backfill_progress",
                (),
            )
            .await
            .unwrap()
            .unwrap(),
            stored_receipt
        );
        let mut source_mismatch: Value = serde_json::from_str(&stored_receipt).unwrap();
        source_mismatch["source_store_id"] = Value::String("foreign-source".to_owned());
        assert!(
            canonical_cutover_replay(
                stored_receipt.clone(),
                &json_text(&source_mismatch).unwrap()
            )
            .is_err(),
            "cutover replay must retain the source store in its identity"
        );
        assert!(
            finalize_memory_v2_cutover(
                &conn,
                &MemoryV2CutoverReceipt::new(
                    receipt_id,
                    owner,
                    source,
                    CapturedMemoryV2Frontiers {
                        facts: frontiers.facts + 1,
                        ..frontiers
                    },
                    UtcMicros(3_000),
                )
                .unwrap(),
            )
            .await
            .is_err(),
            "the same receipt id must still reject a different drained frontier"
        );
    }

    #[tokio::test]
    async fn malformed_and_secret_rows_quarantine_and_advance_without_raw_payload() {
        let (conn, _db, _dir) = database().await;
        conn.execute(
            "INSERT INTO memory_facts(
                fact_id, content, category, tags, trust_score, source, metadata,
                created_at, updated_at
             ) VALUES
                (1, 'malformed canary', 'project', 'not-json', 0.5, 'manual', '{}', 10, 10),
                (2, 'secret canary', 'project', '[]', 0.5, 'manual',
                 '{\"sk-test-123456\":\"raw-quarantine-canary\"}', 10, 10),
                (3, 'survivor', 'project', '[]', 0.5, 'manual', '{}', 10, 10)",
            (),
        )
        .await
        .unwrap();
        let owner = owner();
        let source = source_store_id();
        let frontiers = load_or_capture_memory_v2_frontiers(&conn, &owner, &source)
            .await
            .unwrap();
        run_to_frontier(&conn, &owner, &source, frontiers, 1).await;
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_legacy_quarantine").await,
            2
        );
        assert_eq!(scalar(&conn, "SELECT COUNT(*) FROM memory_facts").await, 1);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
                 WHERE memory_v2_assertion_payloads_fts MATCH '\"raw-quarantine-canary\"'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_current_facts
                 WHERE payload_access = 'quarantined'"
            )
            .await,
            2
        );
    }

    #[tokio::test]
    async fn purge_is_owner_store_fact_scoped_and_clears_payload_fts_and_vectors() {
        let (conn, _db, _dir) = database().await;
        conn.execute(
            "INSERT INTO memory_facts(
                fact_id, content, category, tags, trust_score, source, metadata,
                hrr_vector, created_at, updated_at
             ) VALUES(9, 'purgeable canary', 'project', '[]', 0.5, 'manual', '{}',
                      x'010203', 10, 10)",
            (),
        )
        .await
        .unwrap();
        let owner = owner();
        let source = source_store_id();
        let frontiers = load_or_capture_memory_v2_frontiers(&conn, &owner, &source)
            .await
            .unwrap();
        run_to_frontier(&conn, &owner, &source, frontiers, 4).await;
        let fact_id = FactId::derive(
            &FactIdentityMaterialV1::new(
                owner.clone(),
                FactIdentitySourceV1::Legacy {
                    source_store_id: source.clone(),
                    legacy_fact_id: 9,
                },
            )
            .unwrap(),
        )
        .unwrap();
        let owner_key = owner_key(&owner).unwrap();
        let expected = current_fact_state(&conn, &owner_key, &fact_id)
            .await
            .unwrap()
            .last_event_id;
        assert!(
            purge_memory_v2_fact(
                &conn,
                &owner,
                &source,
                &fact_id,
                &expected,
                UtcMicros(20_000_000),
            )
            .await
            .unwrap()
        );
        assert!(
            purge_memory_v2_fact(
                &conn,
                &owner,
                &source,
                &fact_id,
                &expected,
                UtcMicros(20_000_000),
            )
            .await
            .is_err()
        );
        let deleted = current_fact_state(&conn, &owner_key, &fact_id)
            .await
            .unwrap()
            .last_event_id;
        assert!(
            !purge_memory_v2_fact(
                &conn,
                &owner,
                &source,
                &fact_id,
                &deleted,
                UtcMicros(20_000_000),
            )
            .await
            .unwrap()
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_payloads").await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_vectors").await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
                 WHERE memory_v2_assertion_payloads_fts MATCH 'purgeable'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_facts WHERE fact_id = 9").await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertions").await,
            1
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_legacy_map").await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_current_facts WHERE payload_access = 'deleted'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn purge_clears_runtime_fact_payload_without_a_legacy_mapping() {
        let (conn, _db, _dir) = database().await;
        let owner = owner();
        let owner_key = owner_key(&owner).unwrap();
        let material = FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Application {
                operation_id: ProvenanceId::new("memory-v2.runtime-purge").unwrap(),
            },
        )
        .unwrap();
        let fact_id = FactId::derive(&material).unwrap();
        let identity_json = json_text(&material).unwrap();
        insert_fact_identity(&conn, &owner_key, &fact_id, &identity_json, 10)
            .await
            .unwrap();
        let initial = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Unavailable,
                current: PayloadAccessState::Eligible,
            },
            UtcMicros(10),
            None,
        )
        .unwrap();
        insert_event(&conn, &owner_key, &initial, 10).await.unwrap();
        ensure_current(&conn, &owner_key, &fact_id, initial.event_id(), 10)
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             ) VALUES(
                'assertion.runtime-purge', ?1, ?2, ?3, ?4,
                '{\"assertion_id\":\"assertion.runtime-purge\"}', '{}', '{}', '{}', 10, NULL
             )",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
                owner_key.json.as_str()
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(
                'assertion.runtime-purge', ?1, ?2, ?3,
                '{\"content\":\"runtime-purge-canary\"}', 'runtime-purge-canary'
             )",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_assertion_vectors(
                assertion_id, fact_id, owner_kind, project_id, vector, algebra, dimensions, precision
             ) VALUES(
                'assertion.runtime-purge', ?1, ?2, ?3, x'0102', 'fixture', 2, 'f32'
             )",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await
        .unwrap();

        let source = source_store_id();
        assert!(
            purge_memory_v2_fact(
                &conn,
                &owner,
                &source,
                &fact_id,
                initial.event_id(),
                UtcMicros(20),
            )
            .await
            .unwrap()
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_payloads").await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_vectors").await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
                 WHERE memory_v2_assertion_payloads_fts MATCH '\"runtime-purge-canary\"'"
            )
            .await,
            0
        );
        assert_eq!(
            current_fact_state(&conn, &owner_key, &fact_id)
                .await
                .unwrap()
                .access,
            PayloadAccessState::Deleted
        );
    }
}
