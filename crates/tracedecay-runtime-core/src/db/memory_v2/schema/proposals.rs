//! Proposal-log schema rebuilds, projection indexes, and integrity triggers.

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error, db_message};
use super::introspection::{proposal_schema_is_v22, table_exists, table_has_column};

pub(super) async fn ensure_v22_proposal_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
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

async fn rebuild_v22_proposal_tables(conn: &impl MemoryV2Executor, operation: &str) -> Result<()> {
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
    .map_err(|error| db_error(operation, error))
}

pub(super) async fn install_v21_current_projection_indexes(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
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
    .map_err(|error| db_error(operation, error))
}

pub(super) async fn install_v20_integrity_triggers(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
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
        .map_err(|error| db_error(operation, error))
}
