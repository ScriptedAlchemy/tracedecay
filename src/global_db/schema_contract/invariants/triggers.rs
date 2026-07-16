use libsql::{Connection, params};

use crate::global_db::global_db_operation_error;

use super::rows::authority_violation;
use super::{OPERATION, normalize_trigger_sql};

pub(in crate::global_db::schema_contract) struct Trigger {
    pub(in crate::global_db::schema_contract) name: &'static str,
    pub(in crate::global_db::schema_contract) table: &'static str,
    pub(in crate::global_db::schema_contract) create_sql: &'static str,
}

pub(in crate::global_db::schema_contract) struct Invariant {
    pub(in crate::global_db::schema_contract) triggers: &'static [Trigger],
    pub(super) audit_query: Option<&'static str>,
    pub(super) violation: &'static str,
}

const OBSERVATION_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "observations_immutable_update",
        table: "observations",
        create_sql: "CREATE TRIGGER observations_immutable_update
            BEFORE UPDATE ON observations BEGIN
                SELECT RAISE(ABORT, 'observations are immutable');
            END",
    },
    Trigger {
        name: "observations_immutable_delete",
        table: "observations",
        create_sql: "CREATE TRIGGER observations_immutable_delete
            BEFORE DELETE ON observations BEGIN
                SELECT RAISE(ABORT, 'observations are immutable');
            END",
    },
];

const RECEIPT_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "sanitization_receipts_immutable_update_v1",
        table: "sanitization_receipts",
        create_sql: "CREATE TRIGGER sanitization_receipts_immutable_update_v1
            BEFORE UPDATE ON sanitization_receipts BEGIN
                SELECT RAISE(ABORT, 'sanitization receipts are immutable');
            END",
    },
    Trigger {
        name: "sanitization_receipts_immutable_delete_v1",
        table: "sanitization_receipts",
        create_sql: "CREATE TRIGGER sanitization_receipts_immutable_delete_v1
            BEFORE DELETE ON sanitization_receipts BEGIN
                SELECT RAISE(ABORT, 'sanitization receipts are immutable');
            END",
    },
];

const SOURCE_CURSOR_ADVANCE_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "source_cursor_advances_immutable_update_v1",
        table: "source_cursor_advances",
        create_sql: "CREATE TRIGGER source_cursor_advances_immutable_update_v1
            BEFORE UPDATE ON source_cursor_advances BEGIN
                SELECT RAISE(ABORT, 'source cursor advances are immutable');
            END",
    },
    Trigger {
        name: "source_cursor_advances_immutable_delete_v1",
        table: "source_cursor_advances",
        create_sql: "CREATE TRIGGER source_cursor_advances_immutable_delete_v1
            BEFORE DELETE ON source_cursor_advances BEGIN
                SELECT RAISE(ABORT, 'source cursor advances are immutable');
            END",
    },
];

const PROJECTION_AUDIT_INVALIDATION: &[Trigger] = &[
    Trigger {
        name: "receipt_audit_invalidate_nonappend_insert_v1",
        table: "sanitization_receipts",
        create_sql: "CREATE TRIGGER receipt_audit_invalidate_nonappend_insert_v1
            AFTER INSERT ON sanitization_receipts
            WHEN NEW.rowid <= COALESCE((
                SELECT receipt_rowid FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority'
            ), 0) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "observation_audit_invalidate_nonappend_insert_v1",
        table: "observations",
        create_sql: "CREATE TRIGGER observation_audit_invalidate_nonappend_insert_v1
            AFTER INSERT ON observations
            WHEN NEW.sequence <= COALESCE((
                SELECT observation_sequence FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority'
            ), 0) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "source_cursor_audit_invalidate_key_update_v1",
        table: "source_cursors",
        create_sql: "CREATE TRIGGER source_cursor_audit_invalidate_key_update_v1
            AFTER UPDATE OF source_json, scope_json ON source_cursors BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "source_cursor_audit_invalidate_delete_v1",
        table: "source_cursors",
        create_sql: "CREATE TRIGGER source_cursor_audit_invalidate_delete_v1
            AFTER DELETE ON source_cursors BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_provenance_audit_invalidate_update_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_audit_invalidate_update_v1
            AFTER UPDATE ON observation_projection_provenance BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_provenance_audit_invalidate_delete_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_provenance BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "workflow_fact_audit_invalidate_insert_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_audit_invalidate_insert_v1
            AFTER INSERT ON observation_workflow_facts BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "workflow_fact_audit_invalidate_update_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_audit_invalidate_update_v1
            AFTER UPDATE ON observation_workflow_facts BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "workflow_fact_audit_invalidate_delete_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_audit_invalidate_delete_v1
            AFTER DELETE ON observation_workflow_facts BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_disposition_audit_invalidate_update_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_audit_invalidate_update_v1
            AFTER UPDATE ON observation_projection_dispositions BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_disposition_audit_invalidate_delete_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_dispositions BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_alias_audit_invalidate_update_v1",
        table: "observation_projection_aliases",
        create_sql: "CREATE TRIGGER projection_alias_audit_invalidate_update_v1
            AFTER UPDATE ON observation_projection_aliases BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_alias_audit_invalidate_delete_v1",
        table: "observation_projection_aliases",
        create_sql: "CREATE TRIGGER projection_alias_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_aliases BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_output_audit_invalidate_update_v1",
        table: "session_messages",
        create_sql: "CREATE TRIGGER projection_output_audit_invalidate_update_v1
            AFTER UPDATE ON session_messages
            WHEN EXISTS (
                SELECT 1 FROM observation_projection_provenance
                WHERE projector_version = 'claude-session-message-v3'
                  AND output_provider = OLD.provider
                  AND output_message_id = OLD.message_id
            ) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_output_audit_invalidate_delete_v1",
        table: "session_messages",
        create_sql: "CREATE TRIGGER projection_output_audit_invalidate_delete_v1
            AFTER DELETE ON session_messages
            WHEN EXISTS (
                SELECT 1 FROM observation_projection_provenance
                WHERE projector_version = 'claude-session-message-v3'
                  AND output_provider = OLD.provider
                  AND output_message_id = OLD.message_id
            ) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_checkpoint_audit_invalidate_regression_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_audit_invalidate_regression_v1
            AFTER UPDATE OF last_sequence ON observation_projection_checkpoints
            WHEN NEW.last_sequence < OLD.last_sequence BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_checkpoint_audit_invalidate_delete_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_checkpoints BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
];

const STORE_PROJECT_IMMUTABILITY: &[Trigger] = &[Trigger {
    name: "store_instances_project_immutable_v1",
    table: "store_instances",
    create_sql: "CREATE TRIGGER store_instances_project_immutable_v1
        BEFORE UPDATE OF project_id ON store_instances
        WHEN OLD.project_id IS NOT NEW.project_id
        BEGIN SELECT RAISE(ABORT, 'store project identity is immutable'); END",
}];

const GRAPH_SCOPE_IDENTITY: &[Trigger] = &[
    Trigger {
        name: "graph_scopes_store_project_insert_v1",
        table: "graph_scopes",
        create_sql: "CREATE TRIGGER graph_scopes_store_project_insert_v1
            BEFORE INSERT ON graph_scopes WHEN NOT EXISTS (
                SELECT 1 FROM store_instances
                WHERE store_id = NEW.store_id AND project_id = NEW.project_id
            ) BEGIN SELECT RAISE(ABORT, 'graph scope store/project mismatch'); END",
    },
    Trigger {
        name: "graph_scopes_store_project_update_v1",
        table: "graph_scopes",
        create_sql: "CREATE TRIGGER graph_scopes_store_project_update_v1
            BEFORE UPDATE OF store_id, project_id ON graph_scopes WHEN NOT EXISTS (
                SELECT 1 FROM store_instances
                WHERE store_id = NEW.store_id AND project_id = NEW.project_id
            ) BEGIN SELECT RAISE(ABORT, 'graph scope store/project mismatch'); END",
    },
];

const QUEUE_IDENTITY: &[Trigger] = &[
    Trigger {
        name: "projection_queue_identity_insert_v1",
        table: "projection_queue",
        create_sql: "CREATE TRIGGER projection_queue_identity_insert_v1
            BEFORE INSERT ON projection_queue WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'projection queue observation identity mismatch'); END",
    },
    Trigger {
        name: "projection_queue_identity_update_v1",
        table: "projection_queue",
        create_sql: "CREATE TRIGGER projection_queue_identity_update_v1
            BEFORE UPDATE OF observation_id, observation_sequence ON projection_queue
            WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'projection queue observation identity mismatch'); END",
    },
];

const PROVENANCE_RECEIPT: &[Trigger] = &[
    Trigger {
        name: "projection_provenance_receipt_insert_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_receipt_insert_v1
            BEFORE INSERT ON observation_projection_provenance WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END",
    },
    Trigger {
        name: "projection_provenance_receipt_update_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_receipt_update_v1
            BEFORE UPDATE OF observation_id, receipt_id
            ON observation_projection_provenance WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END",
    },
];

const WORKFLOW_FACT_RECEIPT: &[Trigger] = &[
    Trigger {
        name: "workflow_fact_receipt_insert_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_receipt_insert_v1
            BEFORE INSERT ON observation_workflow_facts WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND receipt_id = NEW.receipt_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'workflow fact observation receipt mismatch'); END",
    },
    Trigger {
        name: "workflow_fact_receipt_update_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_receipt_update_v1
            BEFORE UPDATE OF observation_id, receipt_id, observation_sequence
            ON observation_workflow_facts WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND receipt_id = NEW.receipt_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'workflow fact observation receipt mismatch'); END",
    },
];

const DISPOSITION_RECEIPT: &[Trigger] = &[
    Trigger {
        name: "projection_disposition_receipt_insert_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_receipt_insert_v1
            BEFORE INSERT ON observation_projection_dispositions WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection disposition receipt mismatch'); END",
    },
    Trigger {
        name: "projection_disposition_receipt_update_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_receipt_update_v1
            BEFORE UPDATE OF observation_id, receipt_id
            ON observation_projection_dispositions WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection disposition receipt mismatch'); END",
    },
];

const MESSAGE_CREATED_DOMAIN: &[Trigger] = &[
    Trigger {
        name: "projection_provenance_message_created_insert_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_message_created_insert_v1
            BEFORE INSERT ON observation_projection_provenance
            WHEN NEW.message_created NOT IN (0, 1)
            BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END",
    },
    Trigger {
        name: "projection_provenance_message_created_update_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_message_created_update_v1
            BEFORE UPDATE OF message_created ON observation_projection_provenance
            WHEN NEW.message_created NOT IN (0, 1)
            BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END",
    },
];

const CHECKPOINT_DOMAIN: &[Trigger] = &[
    Trigger {
        name: "projection_checkpoint_sequence_insert_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_sequence_insert_v1
            BEFORE INSERT ON observation_projection_checkpoints
            WHEN NEW.last_sequence < 0
            BEGIN SELECT RAISE(ABORT, 'invalid projection checkpoint sequence'); END",
    },
    Trigger {
        name: "projection_checkpoint_sequence_update_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_sequence_update_v1
            BEFORE UPDATE OF last_sequence ON observation_projection_checkpoints
            WHEN NEW.last_sequence < 0
            BEGIN SELECT RAISE(ABORT, 'invalid projection checkpoint sequence'); END",
    },
];

pub(in crate::global_db::schema_contract) const INVARIANTS: &[Invariant] = &[
    Invariant {
        triggers: OBSERVATION_IMMUTABILITY,
        audit_query: None,
        violation: "observation immutability trigger contract is unavailable",
    },
    Invariant {
        triggers: RECEIPT_IMMUTABILITY,
        audit_query: None,
        violation: "sanitization receipt immutability trigger contract is unavailable",
    },
    Invariant {
        triggers: PROJECTION_AUDIT_INVALIDATION,
        audit_query: None,
        violation: "projection authority audit invalidation contract is unavailable",
    },
    Invariant {
        triggers: STORE_PROJECT_IMMUTABILITY,
        audit_query: None,
        violation: "store project identity is not immutable",
    },
    Invariant {
        triggers: GRAPH_SCOPE_IDENTITY,
        audit_query: Some(
            "SELECT 1 FROM graph_scopes AS scope
             LEFT JOIN store_instances AS store
               ON store.store_id = scope.store_id AND store.project_id = scope.project_id
             WHERE store.store_id IS NULL LIMIT 1",
        ),
        violation: "graph_scopes contains a store/project identity mismatch",
    },
    Invariant {
        triggers: QUEUE_IDENTITY,
        audit_query: Some(
            "SELECT 1 FROM projection_queue AS queue
             LEFT JOIN observations AS observation
               ON observation.observation_id = queue.observation_id
              AND observation.sequence = queue.observation_sequence
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "projection_queue contains an observation identity mismatch",
    },
    Invariant {
        triggers: PROVENANCE_RECEIPT,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_provenance AS provenance
             LEFT JOIN observations AS observation
               ON observation.observation_id = provenance.observation_id
              AND observation.receipt_id = provenance.receipt_id
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "observation projection provenance contains a receipt mismatch",
    },
    Invariant {
        triggers: WORKFLOW_FACT_RECEIPT,
        audit_query: Some(
            "SELECT 1 FROM observation_workflow_facts AS workflow
             LEFT JOIN observations AS observation
               ON observation.observation_id = workflow.observation_id
              AND observation.receipt_id = workflow.receipt_id
              AND observation.sequence = workflow.observation_sequence
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "workflow projection contains an observation receipt mismatch",
    },
    Invariant {
        triggers: DISPOSITION_RECEIPT,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_dispositions AS disposition
             LEFT JOIN observations AS observation
               ON observation.observation_id = disposition.observation_id
              AND observation.receipt_id = disposition.receipt_id
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "observation projection disposition contains a receipt mismatch",
    },
    Invariant {
        triggers: MESSAGE_CREATED_DOMAIN,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_provenance
             WHERE message_created NOT IN (0, 1) LIMIT 1",
        ),
        violation: "observation projection provenance contains invalid message_created",
    },
    Invariant {
        triggers: CHECKPOINT_DOMAIN,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_checkpoints
             WHERE last_sequence < 0 LIMIT 1",
        ),
        violation: "observation projection checkpoints contains a negative sequence",
    },
    Invariant {
        triggers: &[],
        audit_query: Some(
            "SELECT 1 FROM observations AS observation
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = observation.receipt_id
             WHERE receipt.receipt_id IS NULL LIMIT 1",
        ),
        violation: "committed observation references a missing receipt",
    },
    Invariant {
        triggers: &[],
        audit_query: Some(
            "SELECT 1 FROM observations
             WHERE NOT json_valid(observation_json)
                OR NOT json_valid(committed_cursor_json)
                OR (json_type(observation_json, '$.observation_id') IS NOT NULL
                    AND json_extract(observation_json, '$.observation_id') != observation_id)
             LIMIT 1",
        ),
        violation: "committed observation contains invalid authority JSON",
    },
    Invariant {
        triggers: &[],
        audit_query: Some(
            "SELECT 1 FROM observation_projection_checkpoints
             WHERE last_sequence > (SELECT COALESCE(MAX(sequence), 0) FROM observations)
             LIMIT 1",
        ),
        violation: "projection checkpoint exceeds the committed observation frontier",
    },
    Invariant {
        triggers: &[],
        audit_query: Some("PRAGMA foreign_key_check"),
        violation: "global database contains a foreign-key violation",
    },
    Invariant {
        triggers: SOURCE_CURSOR_ADVANCE_IMMUTABILITY,
        audit_query: None,
        violation: "source cursor advance immutability trigger contract is unavailable",
    },
];

pub(super) async fn replace_trigger(
    conn: &Connection,
    trigger: &Trigger,
) -> crate::errors::Result<()> {
    conn.execute(&format!("DROP TRIGGER IF EXISTS \"{}\"", trigger.name), ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    conn.execute_batch(trigger.create_sql)
        .await
        .map(|_| ())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(super) async fn trigger_contracts_intact(conn: &Connection) -> crate::errors::Result<bool> {
    for invariant in INVARIANTS {
        for trigger in invariant.triggers {
            if !trigger_matches(conn, trigger).await? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

async fn trigger_matches(conn: &Connection, trigger: &Trigger) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT tbl_name, sql FROM sqlite_master
             WHERE type = 'trigger' AND name = ?1 COLLATE NOCASE",
            params![trigger.name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    else {
        return Ok(false);
    };
    let table = row
        .get::<String>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let sql = row
        .get::<String>(1)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(table.eq_ignore_ascii_case(trigger.table)
        && normalize_trigger_sql(&sql) == normalize_trigger_sql(trigger.create_sql))
}

pub(in crate::global_db) async fn suspend_immutability_for_canonical_repair(
    conn: &Connection,
) -> crate::errors::Result<()> {
    for trigger in OBSERVATION_IMMUTABILITY.iter().chain(RECEIPT_IMMUTABILITY) {
        if !trigger_matches(conn, trigger).await? {
            return Err(authority_violation(format!(
                "cannot suspend incompatible canonical authority trigger '{}'",
                trigger.name
            )));
        }
    }
    for trigger in OBSERVATION_IMMUTABILITY.iter().chain(RECEIPT_IMMUTABILITY) {
        conn.execute(&format!("DROP TRIGGER \"{}\"", trigger.name), ())
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

pub(in crate::global_db) async fn restore_immutability_after_canonical_repair(
    conn: &Connection,
) -> crate::errors::Result<()> {
    for trigger in OBSERVATION_IMMUTABILITY.iter().chain(RECEIPT_IMMUTABILITY) {
        replace_trigger(conn, trigger).await?;
        if !trigger_matches(conn, trigger).await? {
            return Err(authority_violation(format!(
                "canonical authority trigger '{}' was not restored",
                trigger.name
            )));
        }
    }
    Ok(())
}
