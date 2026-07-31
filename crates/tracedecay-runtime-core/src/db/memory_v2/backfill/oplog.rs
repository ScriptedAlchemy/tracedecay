use tracedecay_domain::{FactOwnerV1, SourceStoreId};

use crate::db::engine::params;
use crate::errors::Result;

use super::super::types::{LegacyOplog, MemoryV2BackfillBatchOutcome, OwnerKey, Progress};
use super::super::writers::{PurgeIntent, insert_quarantine, purge_memory_v2_fact_inner};
use super::super::{
    MemoryV2Executor, OPERATION, db_error, seconds_to_micros, update_cursor, update_phase,
};
use super::facts::ensure_legacy_identity;

pub(in crate::db) async fn backfill_oplog_batch(
    conn: &impl MemoryV2Executor,
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
                    PurgeIntent::ReplayLegacyTombstone,
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
