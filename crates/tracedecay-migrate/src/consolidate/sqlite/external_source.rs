//! Lossless consolidation for owner-bound external-source reducer state.
//!
//! Binding identity, owner, digests, receipts, and encoded reducer state form
//! one integrity unit. Consolidation copies that unit exactly; it never rewrites
//! an old project owner to the destination identity because doing so would
//! invalidate the receipt and frontier digests embedded in `state_json`.

use tracedecay_runtime_core::db::engine::Executor;

use super::{db_message, query_i64, quote_identifier};
use tracedecay_runtime_core::errors::Result;

const COLUMNS: &str = "binding_id, source_id, owner_kind, owner_id,
    definition_digest, binding_digest, frontier_digest,
    receipt_idempotency_key, receipt_request_digest, state_json";

fn divergent_rows(source: &str, target: &str) -> String {
    format!(
        "SELECT COUNT(*)
         FROM {source}.external_source_states_v1 AS s
         JOIN {target}.external_source_states_v1 AS t
           ON t.binding_id = s.binding_id
         WHERE t.source_id IS NOT s.source_id
            OR t.owner_kind IS NOT s.owner_kind
            OR t.owner_id IS NOT s.owner_id
            OR t.definition_digest IS NOT s.definition_digest
            OR t.binding_digest IS NOT s.binding_digest
            OR t.frontier_digest IS NOT s.frontier_digest
            OR t.receipt_idempotency_key IS NOT s.receipt_idempotency_key
            OR t.receipt_request_digest IS NOT s.receipt_request_digest
            OR t.state_json IS NOT s.state_json"
    )
}

async fn preflight(conn: &impl Executor, target_schema: &str, source_schema: &str) -> Result<()> {
    let target = quote_identifier(target_schema);
    let source = quote_identifier(source_schema);
    let collisions = query_i64(conn, &divergent_rows(&source, &target)).await?;
    if collisions == 0 {
        Ok(())
    } else {
        Err(db_message(
            "preflight_external_source_authority",
            format!(
                "{collisions} divergent external source state collision(s); \
                 inputs and backups were preserved"
            ),
        ))
    }
}

pub(super) async fn merge(
    conn: &impl Executor,
    target_schema: &str,
    source_schema: &str,
) -> Result<()> {
    preflight(conn, target_schema, source_schema).await?;
    let target = quote_identifier(target_schema);
    let source = quote_identifier(source_schema);
    let before = query_i64(
        conn,
        &format!("SELECT COUNT(*) FROM {target}.external_source_states_v1"),
    )
    .await?;
    let source_only = query_i64(
        conn,
        &format!(
            "SELECT COUNT(*)
             FROM {source}.external_source_states_v1 AS s
             WHERE NOT EXISTS (
                 SELECT 1 FROM {target}.external_source_states_v1 AS t
                 WHERE t.binding_id = s.binding_id
             )"
        ),
    )
    .await?;
    let expected = before.checked_add(source_only).ok_or_else(|| {
        db_message(
            "merge_external_source_authority",
            "external source state row-count overflow",
        )
    })?;
    conn.execute_batch(&format!(
        "INSERT INTO {target}.external_source_states_v1 ({COLUMNS})
         SELECT {COLUMNS}
         FROM {source}.external_source_states_v1 AS s
         WHERE NOT EXISTS (
             SELECT 1 FROM {target}.external_source_states_v1 AS t
             WHERE t.binding_id = s.binding_id
         );"
    ))
    .await
    .map_err(|error| {
        db_message(
            "merge_external_source_authority",
            format!("external source state union failed: {error}"),
        )
    })?;

    let after = query_i64(
        conn,
        &format!("SELECT COUNT(*) FROM {target}.external_source_states_v1"),
    )
    .await?;
    let missing_or_changed = query_i64(
        conn,
        &format!(
            "SELECT COUNT(*)
             FROM {source}.external_source_states_v1 AS s
             LEFT JOIN {target}.external_source_states_v1 AS t
               ON t.binding_id = s.binding_id
             WHERE t.binding_id IS NULL
                OR t.source_id IS NOT s.source_id
                OR t.owner_kind IS NOT s.owner_kind
                OR t.owner_id IS NOT s.owner_id
                OR t.definition_digest IS NOT s.definition_digest
                OR t.binding_digest IS NOT s.binding_digest
                OR t.frontier_digest IS NOT s.frontier_digest
                OR t.receipt_idempotency_key IS NOT s.receipt_idempotency_key
                OR t.receipt_request_digest IS NOT s.receipt_request_digest
                OR t.state_json IS NOT s.state_json"
        ),
    )
    .await?;
    if after == expected && missing_or_changed == 0 {
        Ok(())
    } else {
        Err(db_message(
            "merge_external_source_authority",
            format!(
                "external source state union verification failed: expected {expected} rows, \
                 observed {after}, with {missing_or_changed} source row difference(s)"
            ),
        ))
    }
}

pub(super) async fn verify_union(
    conn: &impl Executor,
    destination_schema: &str,
    target_schema: &str,
    source_schema: &str,
) -> Result<()> {
    preflight(conn, target_schema, source_schema).await?;
    let destination = quote_identifier(destination_schema);
    let target = quote_identifier(target_schema);
    let source = quote_identifier(source_schema);
    let differences = query_i64(
        conn,
        &format!(
            "WITH expected AS (
                 SELECT {COLUMNS} FROM {target}.external_source_states_v1
                 UNION ALL
                 SELECT {COLUMNS}
                 FROM {source}.external_source_states_v1 AS s
                 WHERE NOT EXISTS (
                     SELECT 1 FROM {target}.external_source_states_v1 AS t
                     WHERE t.binding_id = s.binding_id
                 )
             )
             SELECT
               (SELECT COUNT(*) FROM (
                    SELECT * FROM expected
                    EXCEPT SELECT {COLUMNS}
                           FROM {destination}.external_source_states_v1
                ))
             + (SELECT COUNT(*) FROM (
                    SELECT {COLUMNS} FROM {destination}.external_source_states_v1
                    EXCEPT SELECT * FROM expected
                ))"
        ),
    )
    .await?;
    if differences == 0 {
        Ok(())
    } else {
        Err(db_message(
            "verify_consolidation",
            format!(
                "destination external source state union differs from frozen inputs: \
                 {differences} difference(s)"
            ),
        ))
    }
}
