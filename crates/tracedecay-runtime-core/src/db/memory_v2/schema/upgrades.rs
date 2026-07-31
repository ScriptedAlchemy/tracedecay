//! Versioned schema upgrade and fresh-install entry points (V20..V23).

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error};
use super::baseline::create_schema;
use super::compatibility::{
    install_v22_compatibility_schema, install_v23_compatibility_bank_schema,
    upgrade_v23_fact_relation_schema,
};
use super::introspection::scrub_payload_bearing_assertion_headers;
use super::proposals::{
    add_column_if_missing, ensure_v22_proposal_schema, rebuild_v20_proposal_transition_tables,
    seed_v22_feedback_history_repairs,
};

/// Upgrades the v19 PR7 storage shape without starting a legacy-data
/// backfill.  The caller owns the enclosing exclusive migration transaction.
pub(in crate::db) async fn upgrade_v20_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
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
pub(in crate::db) async fn upgrade_v21_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
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
/// do not call this installer so their `user_version` remains schema-accurate.
pub(in crate::db) async fn upgrade_v22_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await?;
    ensure_v22_proposal_schema(conn, operation).await?;
    seed_v22_feedback_history_repairs(conn, operation).await
}

/// Installs the latest V22 shape for a newly-created database. This is kept
/// separate from the V19 baseline installer because V20/V21 upgrades call the
/// baseline installer while advancing older databases.
pub(in crate::db) async fn install_v22_fresh_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await?;
    ensure_v22_proposal_schema(conn, operation).await
}

/// V23 is deliberately additive from the already-dogfooded V22 shape: it
/// rebuilds the constrained relation projection for full V1 parity, then adds
/// owner-keyed compatibility-bank state. V22 data never relies on a silent
/// latest-schema repair at open time.
pub(in crate::db) async fn upgrade_v23_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    upgrade_v23_fact_relation_schema(conn, operation).await?;
    install_v23_compatibility_bank_schema(conn, operation).await
}

/// Installs V23 over a fresh V22 baseline. Keeping this explicit makes a
/// newly-created database match the same V22-to-V23 contract used by durable
/// dogfood databases.
pub(in crate::db) async fn install_v23_fresh_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    upgrade_v23_schema(conn, operation).await
}
