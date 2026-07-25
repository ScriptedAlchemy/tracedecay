use tracedecay_domain::{FactOwnerV1, SourceStoreId};

use crate::db::engine::{self, params};
use crate::errors::Result;

use super::backfill::{backfill_fact_batch, backfill_feedback_batch, backfill_oplog_batch};
use super::types::{
    CapturedMemoryV2Frontiers, MemoryV2BackfillBatchOutcome, MemoryV2CutoverOutcome,
    MemoryV2CutoverReceipt,
};
use super::{
    MAX_BATCH_SIZE, OPERATION, begin, canonical_cutover_replay, db_error, db_message,
    finish_transaction, json_text, load_or_create_progress, now_micros, owner_key, scalar_i64,
    validate_scope, validate_v1_compatibility_source,
};

pub(in crate::db) async fn load_or_capture_memory_v2_frontiers(
    conn: &engine::Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
) -> Result<CapturedMemoryV2Frontiers> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    let transaction = begin(conn, "memory_v2_capture_frontiers").await?;
    let result = {
        let conn = &transaction;
        async {
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
                facts: scalar_i64(conn, "SELECT COALESCE(MAX(fact_id), 0) FROM memory_facts")
                    .await?,
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
        .await
    };
    finish_transaction(transaction, result, "memory_v2_capture_frontiers").await
}

/// Processes at most one bounded source-table batch. Captured frontiers are
/// immutable job identity: retries with shifted frontiers fail closed.
pub(in crate::db) async fn backfill_memory_v2_batch(
    conn: &engine::Connection,
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
    let transaction = begin(conn, OPERATION).await?;
    let result = {
        let conn = &transaction;
        async {
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
        .await
    };
    finish_transaction(transaction, result, OPERATION).await
}

pub(in crate::db) async fn finalize_memory_v2_cutover(
    conn: &engine::Connection,
    receipt: &MemoryV2CutoverReceipt,
) -> Result<MemoryV2CutoverOutcome> {
    validate_scope(&receipt.owner, &receipt.source_store_id)?;
    validate_v1_compatibility_source(&receipt.source_store_id)?;
    let owner = owner_key(&receipt.owner)?;
    let receipt_json = json_text(receipt)?;
    let transaction = begin(conn, "memory_v2_cutover").await?;
    let result = {
        let conn = &transaction;
        async {
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
                facts: scalar_i64(conn, "SELECT COALESCE(MAX(fact_id), 0) FROM memory_facts")
                    .await?,
            };
            if tail.feedback > stored.feedback
                || tail.oplog > stored.oplog
                || tail.facts > stored.facts
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
        .await
    };
    finish_transaction(transaction, result, "memory_v2_cutover").await
}

/// Restarts legacy projection after an offline branch-memory union.
///
/// Feedback/oplog cursors remain valid because unioned rows receive ids above
/// the existing project maxima. Facts always replay from zero: an all-duplicate
/// union may update trust, counters, metadata, or vectors without increasing
/// the fact frontier, and branch-local numeric ids may have been remapped.
pub(in crate::db) async fn reopen_memory_v2_cutover_for_legacy_union(
    conn: &engine::Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
) -> Result<bool> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    let owner = owner_key(owner)?;
    let transaction = begin(conn, "memory_v2_reopen_cutover").await?;
    let result = {
        let conn = &transaction;
        async {
            let mut rows = conn
                .query(
                    "SELECT phase
                     FROM memory_v2_backfill_progress
                     WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
                    params![
                        owner.kind,
                        owner.project_id.as_str(),
                        source_store_id.as_str()
                    ],
                )
                .await
                .map_err(|error| db_error("memory_v2_reopen_cutover", error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| db_error("memory_v2_reopen_cutover", error))?
            else {
                return Ok(false);
            };
            let phase: String = row
                .get(0)
                .map_err(|error| db_error("memory_v2_reopen_cutover", error))?;
            drop(rows);
            if !matches!(
                phase.as_str(),
                "feedback" | "oplog" | "facts" | "awaiting_cutover" | "cutover_complete"
            ) {
                return Err(db_message(
                    "memory_v2_reopen_cutover",
                    "stored backfill phase is invalid",
                ));
            }
            let tail = CapturedMemoryV2Frontiers {
                feedback: scalar_i64(
                    conn,
                    "SELECT COALESCE(MAX(event_id), 0) FROM memory_feedback_events",
                )
                .await?,
                oplog: scalar_i64(conn, "SELECT COALESCE(MAX(id), 0) FROM memory_oplog").await?,
                facts: scalar_i64(conn, "SELECT COALESCE(MAX(fact_id), 0) FROM memory_facts")
                    .await?,
            };
            conn.execute(
                "UPDATE memory_v2_backfill_progress SET
                    phase = 'feedback',
                    feedback_frontier = MAX(feedback_frontier, ?1),
                    oplog_frontier = MAX(oplog_frontier, ?2),
                    fact_frontier = MAX(fact_frontier, ?3),
                    fact_cursor = 0,
                    cutover_completed_at = NULL,
                    cutover_receipt_json = NULL,
                    updated_at = ?4
                 WHERE owner_kind = ?5 AND project_id = ?6 AND source_store_id = ?7",
                params![
                    tail.feedback,
                    tail.oplog,
                    tail.facts,
                    now_micros()?,
                    owner.kind,
                    owner.project_id.as_str(),
                    source_store_id.as_str()
                ],
            )
            .await
            .map_err(|error| db_error("memory_v2_reopen_cutover", error))?;
            Ok(true)
        }
        .await
    };
    finish_transaction(transaction, result, "memory_v2_reopen_cutover").await
}
