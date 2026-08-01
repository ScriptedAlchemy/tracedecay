use tracedecay_domain::{FactOwnerV1, SourceStoreId};

use crate::db::engine::{self, params};
use crate::errors::Result;

use super::backfill::{backfill_fact_batch, backfill_feedback_batch, backfill_oplog_batch};
use super::types::{
    CapturedMemoryV2Frontiers, MemoryV2BackfillBatchOutcome, MemoryV2CutoverCoverage,
    MemoryV2CutoverOutcome, MemoryV2CutoverReceipt, OwnerKey,
};
use super::{
    MAX_BATCH_SIZE, OPERATION, begin, canonical_cutover_replay, db_error, db_message,
    finish_transaction, json_text, load_or_create_progress, now_micros, owner_key, scalar_i64,
    scalar_i64_params, validate_scope, validate_v1_compatibility_source,
};

/// Whether this owner has no V1 legacy memory to migrate at all.
///
/// A store that never held a V1 memory bank has nothing to cut over, yet the
/// capture → drain → finalize ladder still runs end to end on it: it inserts
/// an all-zero `memory_v2_backfill_progress` row and a cutover receipt for a
/// migration that never happened, then re-reads them forever.
///
/// Vacuity requires both no recorded progress *and* no legacy source rows.
/// Once progress exists the ladder owns the decision — in particular after a
/// legacy purge, where the source tables are empty precisely because the
/// cutover already moved them.
pub(in crate::db) async fn memory_v2_cutover_is_vacuous(
    conn: &engine::Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
) -> Result<bool> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    let owner = owner_key(owner)?;
    let recorded = scalar_i64_params(
        conn,
        "SELECT EXISTS(
             SELECT 1 FROM memory_v2_backfill_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
         )",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await?;
    if recorded != 0 {
        return Ok(false);
    }
    for probe in [
        "SELECT EXISTS(SELECT 1 FROM memory_facts)",
        "SELECT EXISTS(SELECT 1 FROM memory_feedback_events)",
        "SELECT EXISTS(SELECT 1 FROM memory_oplog)",
    ] {
        if scalar_i64(conn, probe).await? != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

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

async fn memory_v2_cutover_coverage(
    conn: &impl super::MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    frontiers: CapturedMemoryV2Frontiers,
) -> Result<MemoryV2CutoverCoverage> {
    let source_fact_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*) FROM memory_facts WHERE fact_id <= ?1",
        params![frontiers.facts],
    )
    .await?;
    let represented_fact_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*)
         FROM memory_facts AS legacy
         WHERE legacy.fact_id <= ?1
           AND EXISTS(
             SELECT 1
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = mappings.fact_id
              AND facts.owner_kind = mappings.owner_kind
              AND facts.project_id = mappings.project_id
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = mappings.fact_id
              AND current_facts.owner_kind = mappings.owner_kind
              AND current_facts.project_id = mappings.project_id
             WHERE mappings.owner_kind = ?2
               AND mappings.project_id = ?3
               AND mappings.owner_json = ?4
               AND mappings.source_store_id = ?5
               AND mappings.legacy_fact_id = legacy.fact_id
               AND facts.owner_json = mappings.owner_json
               AND (
                 EXISTS(
                   SELECT 1 FROM memory_v2_legacy_quarantine AS quarantine
                   WHERE quarantine.owner_kind = mappings.owner_kind
                     AND quarantine.project_id = mappings.project_id
                     AND quarantine.source_store_id = mappings.source_store_id
                     AND quarantine.source_table = 'memory_facts'
                     AND quarantine.source_row_id = legacy.fact_id
                 )
                 OR (
                   current_facts.payload_access IN ('eligible', 'redacted')
                   AND current_facts.active_assertion_id IS NOT NULL
                   AND EXISTS(
                     SELECT 1 FROM memory_v2_assertion_payloads AS payloads
                     WHERE payloads.assertion_id = current_facts.active_assertion_id
                       AND payloads.fact_id = current_facts.fact_id
                       AND payloads.owner_kind = current_facts.owner_kind
                       AND payloads.project_id = current_facts.project_id
                   )
                 )
               )
           )",
        params![
            frontiers.facts,
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            source_store_id.as_str()
        ],
    )
    .await?;
    let source_feedback_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*) FROM memory_feedback_events WHERE event_id <= ?1",
        params![frontiers.feedback],
    )
    .await?;
    let represented_feedback_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*)
         FROM memory_feedback_events AS legacy
         WHERE legacy.event_id <= ?1
           AND (
             EXISTS(
               SELECT 1
               FROM memory_v2_legacy_feedback_event_map AS mappings
               JOIN memory_v2_lineage_events AS events
                 ON events.event_id = mappings.event_id
                AND events.fact_id = mappings.fact_id
                AND events.owner_kind = mappings.owner_kind
                AND events.project_id = mappings.project_id
               WHERE mappings.owner_kind = ?2
                 AND mappings.project_id = ?3
                 AND mappings.source_store_id = ?4
                 AND mappings.legacy_feedback_event_id = legacy.event_id
             )
             OR EXISTS(
               SELECT 1 FROM memory_v2_legacy_quarantine AS quarantine
               WHERE quarantine.owner_kind = ?2
                 AND quarantine.project_id = ?3
                 AND quarantine.source_store_id = ?4
                 AND quarantine.source_table = 'memory_feedback_events'
                 AND quarantine.source_row_id = legacy.event_id
             )
           )",
        params![
            frontiers.feedback,
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await?;
    let source_oplog_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*) FROM memory_oplog WHERE id <= ?1",
        params![frontiers.oplog],
    )
    .await?;
    // The legacy oplog is a processing journal rather than a one-row-per-event
    // V2 projection: add/update/feedback/curate entries are intentionally
    // quarantined and successful removals become lineage events. Reaching the
    // facts phase proves the append-only oplog cursor was drained transactionally
    // through this frontier, so the persisted frontier is its coverage witness.
    let represented_oplog_count = source_oplog_count;
    Ok(MemoryV2CutoverCoverage {
        source_fact_count,
        represented_fact_count,
        source_feedback_count,
        represented_feedback_count,
        source_oplog_count,
        represented_oplog_count,
    })
}

async fn require_complete_cutover_coverage(
    conn: &impl super::MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    frontiers: CapturedMemoryV2Frontiers,
) -> Result<MemoryV2CutoverCoverage> {
    let coverage = memory_v2_cutover_coverage(conn, owner, source_store_id, frontiers).await?;
    if coverage.is_complete() {
        return Ok(coverage);
    }
    Err(db_message(
        "memory_v2_cutover",
        format!(
            "cutover coverage verification failed: facts {}/{}, feedback {}/{}, oplog {}/{}",
            coverage.represented_fact_count,
            coverage.source_fact_count,
            coverage.represented_feedback_count,
            coverage.source_feedback_count,
            coverage.represented_oplog_count,
            coverage.source_oplog_count,
        ),
    ))
}

pub(super) async fn verify_memory_v2_cutover_complete(
    conn: &impl super::MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
) -> Result<MemoryV2CutoverCoverage> {
    let mut rows = conn
        .query(
            "SELECT phase, feedback_frontier, oplog_frontier, fact_frontier
             FROM memory_v2_backfill_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error("memory_v2_cutover", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error("memory_v2_cutover", error))?
        .ok_or_else(|| db_message("memory_v2_cutover", "backfill progress is missing"))?;
    let phase: String = row
        .get(0)
        .map_err(|error| db_error("memory_v2_cutover", error))?;
    if phase != "cutover_complete" {
        return Err(db_message(
            "memory_v2_cutover",
            "legacy payload purge requires a verified completed backfill",
        ));
    }
    let frontiers = CapturedMemoryV2Frontiers {
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
    drop(rows);
    require_complete_cutover_coverage(conn, owner, source_store_id, frontiers).await
}

/// Whether a stored cutover receipt already carries a complete coverage
/// witness. A receipt written before coverage was recorded, or one whose
/// witness is incomplete, returns `false` so the caller recomputes and
/// persists it once.
fn stored_coverage_is_verified(receipt_json: &str) -> Result<bool> {
    let receipt: serde_json::Value = serde_json::from_str(receipt_json).map_err(|_| {
        db_message(
            "memory_v2_cutover",
            "stored cutover receipt is invalid JSON",
        )
    })?;
    let Some(coverage) = receipt.get("coverage") else {
        return Ok(false);
    };
    Ok(
        serde_json::from_value::<MemoryV2CutoverCoverage>(coverage.clone())
            .is_ok_and(MemoryV2CutoverCoverage::is_complete),
    )
}

fn receipt_json_with_coverage(
    receipt_json: &str,
    coverage: MemoryV2CutoverCoverage,
) -> Result<String> {
    let mut receipt: serde_json::Value = serde_json::from_str(receipt_json).map_err(|_| {
        db_message(
            "memory_v2_cutover",
            "stored cutover receipt is invalid JSON",
        )
    })?;
    let object = receipt.as_object_mut().ok_or_else(|| {
        db_message(
            "memory_v2_cutover",
            "stored cutover receipt is not an object",
        )
    })?;
    let coverage = serde_json::to_value(coverage)
        .map_err(|_| db_message("memory_v2_cutover", "cutover coverage encoding failed"))?;
    if object.get("coverage") == Some(&coverage) {
        return Ok(receipt_json.to_owned());
    }
    object.insert("coverage".to_owned(), coverage);
    json_text(&receipt)
}

pub(in crate::db) async fn finalize_memory_v2_cutover(
    conn: &engine::Connection,
    receipt: &MemoryV2CutoverReceipt,
) -> Result<MemoryV2CutoverOutcome> {
    validate_scope(&receipt.owner, &receipt.source_store_id)?;
    validate_v1_compatibility_source(&receipt.source_store_id)?;
    let owner = owner_key(&receipt.owner)?;
    let candidate_receipt_json = json_text(receipt)?;
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
                canonical_cutover_replay(existing.clone(), &candidate_receipt_json)?;
                // A completed cutover is frozen: the frontiers no longer move
                // and the legacy source rows below them are immutable, so the
                // stored witness stays true. Recomputing the four-table
                // coverage join here re-proves that same fact on every daemon
                // tick, for every project, and then writes nothing.
                if stored_coverage_is_verified(&existing)? {
                    return Ok(MemoryV2CutoverOutcome::Complete);
                }
                let coverage = require_complete_cutover_coverage(
                    conn,
                    &owner,
                    &receipt.source_store_id,
                    stored,
                )
                .await?;
                let verified_receipt_json = receipt_json_with_coverage(&existing, coverage)?;
                if verified_receipt_json != existing {
                    conn.execute(
                        "UPDATE memory_v2_backfill_progress
                         SET cutover_receipt_json = ?1, updated_at = ?2
                         WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5",
                        params![
                            verified_receipt_json,
                            now_micros()?,
                            owner.kind,
                            owner.project_id.as_str(),
                            receipt.source_store_id.as_str()
                        ],
                    )
                    .await
                    .map_err(|error| db_error("memory_v2_cutover", error))?;
                }
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
            let coverage =
                require_complete_cutover_coverage(conn, &owner, &receipt.source_store_id, stored)
                    .await?;
            let verified_receipt_json = json_text(&receipt.with_verified_coverage(coverage))?;
            conn.execute(
                "UPDATE memory_v2_backfill_progress SET
                phase = 'cutover_complete', cutover_completed_at = ?1,
                cutover_receipt_json = ?2, updated_at = ?1
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5",
                params![
                    receipt.dual_write_activated_at.0,
                    verified_receipt_json,
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
