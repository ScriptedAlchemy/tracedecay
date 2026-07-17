use libsql::{Connection, params};
use serde_json::{Value, json};

use crate::errors::Result;

use super::{
    OPERATION, V1_COMPATIBILITY_SOURCE_STORE, db_error, db_message, json_text, now_micros,
    optional_string, row_exists,
};

/// Installs only additive storage. Legacy data movement is daemon-authorized
/// and deliberately absent from bare schema creation and database open.
pub(crate) async fn create_schema(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error(operation, error))?;
    super::super::retrieval_anchor_schema::install_retrieval_anchor_schema(conn, operation).await?;
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
pub(crate) async fn upgrade_v20_schema(conn: &Connection, operation: &str) -> Result<()> {
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
pub(crate) async fn upgrade_v21_schema(conn: &Connection, operation: &str) -> Result<()> {
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
pub(crate) async fn upgrade_v22_schema(conn: &Connection, operation: &str) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await?;
    ensure_v22_proposal_schema(conn, operation).await?;
    seed_v22_feedback_history_repairs(conn, operation).await
}

/// Installs the latest V22 shape for a newly-created database. This is kept
/// separate from the V19 baseline installer because V20/V21 upgrades call the
/// baseline installer while advancing older databases.
pub(crate) async fn install_v22_fresh_schema(conn: &Connection, operation: &str) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await?;
    ensure_v22_proposal_schema(conn, operation).await
}

/// V23 is deliberately additive from the already-dogfooded V22 shape: it
/// rebuilds the constrained relation projection for full V1 parity, then adds
/// owner-keyed compatibility-bank state. V22 data never relies on a silent
/// latest-schema repair at open time.
pub(crate) async fn upgrade_v23_schema(conn: &Connection, operation: &str) -> Result<()> {
    upgrade_v23_fact_relation_schema(conn, operation).await?;
    install_v23_compatibility_bank_schema(conn, operation).await
}

/// Installs V23 over a fresh V22 baseline. Keeping this explicit makes a
/// newly-created database match the same V22-to-V23 contract used by durable
/// dogfood databases.
pub(crate) async fn install_v23_fresh_schema(conn: &Connection, operation: &str) -> Result<()> {
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

pub(super) async fn table_has_column(
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

pub(super) async fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
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
pub(super) async fn v22_feedback_history_schema_installed(conn: &Connection) -> Result<bool> {
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

pub(super) async fn proposal_schema_is_v22(conn: &Connection) -> Result<bool> {
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
