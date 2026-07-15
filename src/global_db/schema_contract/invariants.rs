use libsql::{Connection, TransactionBehavior, params};
use tracedecay_store::CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION;

use super::super::{global_db_operation_error, global_db_operation_message};

const OPERATION: &str = "ensure global database authority invariants";

pub(super) struct Trigger {
    pub(super) name: &'static str,
    pub(super) table: &'static str,
    create_sql: &'static str,
}

pub(super) struct Invariant {
    pub(super) triggers: &'static [Trigger],
    audit_query: Option<&'static str>,
    violation: &'static str,
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

pub(super) const INVARIANTS: &[Invariant] = &[
    Invariant {
        triggers: OBSERVATION_IMMUTABILITY,
        audit_query: None,
        violation: "observation immutability trigger contract is unavailable",
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
];

async fn query_has_rows(conn: &Connection, query: &str) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(query, ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

async fn repair_projection_frontier(conn: &Connection) -> crate::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let checkpoint = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .map(|row| row.get::<i64>(0))
        .transpose()
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .unwrap_or(0);
    drop(rows);

    conn.execute(
        "DELETE FROM projection_queue
         WHERE observation_sequence <= ?1
            OR NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observations.observation_id = projection_queue.observation_id
                  AND observations.sequence = projection_queue.observation_sequence
            )",
        params![checkpoint],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    conn.execute(
        "INSERT OR IGNORE INTO projection_queue (observation_id, observation_sequence)
         SELECT observation_id, sequence FROM observations WHERE sequence > ?1",
        params![checkpoint],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(())
}

async fn repair_committed_source_cursors(conn: &Connection) -> crate::errors::Result<()> {
    conn.execute(
        "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
         SELECT source_json, scope_json, cursor_json
         FROM (
            SELECT json(json_extract(observation_json, '$.identity.source')) AS source_json,
                   json(json_extract(observation_json, '$.identity.scope')) AS scope_json,
                   committed_cursor_json AS cursor_json,
                   ROW_NUMBER() OVER (
                       PARTITION BY json(json_extract(observation_json, '$.identity.source')),
                                    json(json_extract(observation_json, '$.identity.scope'))
                       ORDER BY sequence DESC
                   ) AS recency
            FROM observations
            WHERE json_valid(observation_json)
              AND json_type(observation_json, '$.identity.source') IS NOT NULL
              AND json_type(observation_json, '$.identity.scope') IS NOT NULL
              AND json_valid(committed_cursor_json)
         )
         WHERE recency = 1
         ON CONFLICT(source_json, scope_json) DO UPDATE SET
            cursor_json = excluded.cursor_json",
        (),
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OPERATION, error))
}

async fn replace_trigger(conn: &Connection, trigger: &Trigger) -> crate::errors::Result<()> {
    conn.execute(&format!("DROP TRIGGER IF EXISTS \"{}\"", trigger.name), ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    conn.execute_batch(trigger.create_sql)
        .await
        .map(|_| ())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(in crate::global_db) async fn ensure_authority_invariants(
    conn: &Connection,
) -> crate::errors::Result<()> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    repair_committed_source_cursors(&transaction).await?;
    repair_projection_frontier(&transaction).await?;
    for invariant in INVARIANTS {
        for trigger in invariant.triggers {
            replace_trigger(&transaction, trigger).await?;
        }
        if let Some(query) = invariant.audit_query
            && query_has_rows(&transaction, query).await?
        {
            return Err(global_db_operation_message(OPERATION, invariant.violation));
        }
    }
    transaction
        .commit()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(super) async fn validate_invariant_rows(conn: &Connection) -> crate::errors::Result<()> {
    for invariant in INVARIANTS {
        if let Some(query) = invariant.audit_query
            && query_has_rows(conn, query).await?
        {
            return Err(global_db_operation_message(OPERATION, invariant.violation));
        }
    }
    Ok(())
}
