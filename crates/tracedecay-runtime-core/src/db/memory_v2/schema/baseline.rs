//! V19 additive baseline schema installer.

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error};
use super::proposals::{install_v20_integrity_triggers, install_v21_current_projection_indexes};

/// Installs only additive storage. Legacy data movement is daemon-authorized
/// and deliberately absent from bare schema creation and database open.
pub(in crate::db) async fn create_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error(operation, error))?;
    crate::db::retrieval_anchor_schema::install_retrieval_anchor_schema(conn, operation).await?;
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
    install_v20_integrity_triggers(conn, operation).await?;
    install_v21_current_projection_indexes(conn, operation).await?;
    Ok(())
}
