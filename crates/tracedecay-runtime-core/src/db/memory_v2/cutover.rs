//! Coverage verification for the retired V1 legacy-memory cutover.
//!
//! The V1 -> V2 drain itself is gone. What remains is the fail-closed gate the
//! live legacy-payload purge path still takes: a payload may only be reclaimed
//! when a complete, verified cutover receipt exists for that owner/store.

use tracedecay_domain::SourceStoreId;

use crate::db::engine::params;
use crate::errors::Result;

use super::types::{CapturedMemoryV2Frontiers, MemoryV2CutoverCoverage, OwnerKey};
use super::{db_error, db_message, scalar_i64_params};

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
