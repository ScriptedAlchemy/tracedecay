use tracedecay_domain::{
    Confidence, FactId, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, SourceStoreId,
};

#[cfg(test)]
use crate::db::engine;
use crate::db::engine::params;
use crate::errors::Result;

use super::types::{
    FeedbackHistoryRepairProgress, LegacyFeedback, MemoryV2FeedbackHistoryRepairBatchOutcome,
    MemoryV2FeedbackHistoryRepairProgress, OwnerKey,
};
use super::writers::{
    insert_feedback_history, insert_legacy_feedback_event_mapping, insert_quarantine,
    legacy_feedback_mapping_can_be_recorded,
};
use super::{
    MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE, MemoryV2Executor, OPERATION, db_error, db_message,
    now_micros, optional_string, owner_key, row_exists, sanitize_legacy_feedback_details,
    seconds_to_micros, validate_scope, validate_v1_compatibility_source,
};
#[cfg(test)]
use super::{begin, finish_transaction};

/// Returns the V22-owned repair snapshot for an owner/source, if that owner
/// had feedback already imported before V22 history projections existed.
pub(in crate::db) async fn feedback_history_repair_progress(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
) -> Result<Option<MemoryV2FeedbackHistoryRepairProgress>> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    let owner = owner_key(owner)?;
    let Some(progress) =
        load_feedback_history_repair_progress(conn, &owner, source_store_id).await?
    else {
        return Ok(None);
    };
    Ok(Some(MemoryV2FeedbackHistoryRepairProgress {
        feedback_frontier: progress.feedback_frontier,
        feedback_cursor: progress.feedback_cursor,
        complete: progress.phase == "complete",
    }))
}

/// Repairs at most one V22-owned, captured legacy-feedback batch. It only
/// creates mapping/history projections for lineage events V1 had already
/// imported. Rows without an owner-matched legacy mapping are excluded because
/// V1 feedback is unscoped; eligible malformed rows are quarantined and still
/// advance.
///
/// Standalone transaction wrapper retained for owner-bound batch tests; the
/// production repair tick drives `*_in_transaction` inside a caller-owned
/// authority transaction.
#[cfg(test)]
pub(super) async fn repair_memory_v2_feedback_history_batch(
    conn: &engine::Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<MemoryV2FeedbackHistoryRepairBatchOutcome> {
    let owner_key = feedback_history_repair_owner_key(owner, source_store_id, batch_size)?;
    let transaction = begin(conn, "memory_v2_feedback_history_repair").await?;
    let result = repair_memory_v2_feedback_history_batch_inner(
        &transaction,
        owner,
        &owner_key,
        source_store_id,
        batch_size,
    )
    .await;
    finish_transaction(transaction, result, "memory_v2_feedback_history_repair").await
}

/// Repairs one bounded V22 feedback-history batch inside the caller's
/// authoritative writer transaction. This never starts or finishes a nested
/// transaction, so the projection, V1 repair, and operation receipt can commit
/// or roll back together.
pub(in crate::db) async fn repair_memory_v2_feedback_history_batch_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<MemoryV2FeedbackHistoryRepairBatchOutcome> {
    let owner_key = feedback_history_repair_owner_key(owner, source_store_id, batch_size)?;
    repair_memory_v2_feedback_history_batch_inner(
        conn,
        owner,
        &owner_key,
        source_store_id,
        batch_size,
    )
    .await
}

fn feedback_history_repair_owner_key(
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<OwnerKey> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    if !(1..=MAX_FEEDBACK_HISTORY_REPAIR_BATCH_SIZE).contains(&batch_size) {
        return Err(db_message(
            OPERATION,
            "feedback history repair batch size is outside the bounded range",
        ));
    }
    owner_key(owner)
}

async fn repair_memory_v2_feedback_history_batch_inner(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    batch_size: i64,
) -> Result<MemoryV2FeedbackHistoryRepairBatchOutcome> {
    let Some(progress) =
        load_feedback_history_repair_progress(conn, owner_key, source_store_id).await?
    else {
        return Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::NotRequired);
    };
    match progress.phase.as_str() {
        "complete" => {
            return Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 0 });
        }
        "pending" => {}
        _ => {
            return Err(db_message(
                OPERATION,
                "stored feedback repair phase is invalid",
            ));
        }
    }

    let batch = load_owner_legacy_feedback_repair_batch(
        conn,
        owner_key,
        source_store_id,
        progress.feedback_cursor,
        progress.feedback_frontier,
        batch_size,
    )
    .await?;
    if batch.is_empty() {
        complete_feedback_history_repair(
            conn,
            owner_key,
            source_store_id,
            progress.feedback_frontier,
        )
        .await?;
        return Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed: 0 });
    }
    for item in &batch {
        repair_legacy_feedback_history_item(
            conn,
            owner,
            owner_key,
            source_store_id,
            &progress,
            item,
        )
        .await?;
    }
    let cursor = batch
        .last()
        .map_or(progress.feedback_cursor, |item| item.event_id);
    if cursor >= progress.feedback_frontier {
        complete_feedback_history_repair(conn, owner_key, source_store_id, cursor).await?;
        Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Complete {
            processed: batch.len(),
        })
    } else {
        advance_feedback_history_repair(conn, owner_key, source_store_id, cursor).await?;
        Ok(MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced {
            processed: batch.len(),
        })
    }
}

/// V22 repair only revisits feedback whose legacy fact already belongs to the
/// exact owner. The V1 source tables are unscoped, so scanning them directly
/// would quarantine or project another owner's rows.
async fn load_owner_legacy_feedback_repair_batch(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    cursor: i64,
    frontier: i64,
    limit: i64,
) -> Result<Vec<LegacyFeedback>> {
    let mut rows = conn
        .query(
            "SELECT feedback.event_id, feedback.fact_id, feedback.action,
                    feedback.old_trust, feedback.new_trust, feedback.created_at,
                    feedback.source, feedback.note
             FROM memory_feedback_events AS feedback
             JOIN memory_v2_legacy_map AS mapping
               ON mapping.legacy_fact_id = feedback.fact_id
              AND mapping.owner_kind = ?4
              AND mapping.project_id = ?5
              AND mapping.source_store_id = ?6
             WHERE feedback.event_id > ?1 AND feedback.event_id <= ?2
             ORDER BY feedback.event_id LIMIT ?3",
            params![
                cursor,
                frontier,
                limit,
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyFeedback {
            event_id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            fact_id: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            action: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            old_trust: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            new_trust: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            created_at: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            source: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            note: row.get(7).map_err(|error| db_error(OPERATION, error))?,
        });
    }
    Ok(batch)
}

async fn repair_legacy_feedback_history_item(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    progress: &FeedbackHistoryRepairProgress,
    item: &LegacyFeedback,
) -> Result<()> {
    let action = match item.action.as_str() {
        "helpful" => "helpful",
        "unhelpful" => "unhelpful",
        _ => {
            return insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "unknown_feedback_action",
                progress.started_at,
            )
            .await;
        }
    };
    let (Ok(previous), Ok(current), Some(occurred_at)) = (
        Confidence::new(item.old_trust),
        Confidence::new(item.new_trust),
        seconds_to_micros(item.created_at),
    ) else {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "invalid_feedback_contract",
            progress.started_at,
        )
        .await;
    };
    if previous == current {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "non_transition_feedback",
            progress.started_at,
        )
        .await;
    }
    if (action == "helpful" && current <= previous)
        || (action == "unhelpful" && current >= previous)
    {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_action_direction_mismatch",
            progress.started_at,
        )
        .await;
    }
    let Some(mapped_fact_id) = optional_string(
        conn,
        "SELECT fact_id FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND legacy_fact_id = ?4",
        params![
            owner_key.kind,
            owner_key.project_id.as_str(),
            source_store_id.as_str(),
            item.fact_id
        ],
    )
    .await?
    else {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_mapping_unavailable",
            progress.started_at,
        )
        .await;
    };
    let Ok(fact_id) = FactId::new(mapped_fact_id) else {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_mapping_invalid",
            progress.started_at,
        )
        .await;
    };
    let Ok(event) = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::TrustChanged {
            previous,
            current,
            evidence_ids: Vec::new(),
        },
        occurred_at,
        None,
    ) else {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_lineage_unavailable",
            progress.started_at,
        )
        .await;
    };
    if !row_exists(
        conn,
        "SELECT 1 FROM memory_v2_lineage_events
         WHERE event_id = ?1 AND fact_id = ?2 AND owner_kind = ?3 AND project_id = ?4",
        params![
            event.event_id().as_str(),
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await?
    {
        return insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            "feedback_lineage_unavailable",
            progress.started_at,
        )
        .await;
    }
    if !legacy_feedback_mapping_can_be_recorded(
        conn,
        owner_key,
        source_store_id,
        item.event_id,
        &fact_id,
        event.event_id(),
        progress.started_at,
    )
    .await?
    {
        return Ok(());
    }
    let (source, note, details_availability, quarantine_reason) =
        sanitize_legacy_feedback_details(item.source.as_deref(), item.note.as_deref());
    if let Some(reason) = quarantine_reason {
        insert_quarantine(
            conn,
            owner_key,
            source_store_id,
            "memory_feedback_events",
            item.event_id,
            reason,
            progress.started_at,
        )
        .await?;
    }
    insert_legacy_feedback_event_mapping(
        conn,
        owner_key,
        source_store_id,
        item.event_id,
        &fact_id,
        event.event_id(),
    )
    .await?;
    insert_feedback_history(
        conn,
        owner_key,
        &fact_id,
        event.event_id(),
        action,
        previous,
        current,
        occurred_at,
        source.as_deref(),
        note.as_deref(),
        details_availability,
    )
    .await
}

async fn load_feedback_history_repair_progress(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
) -> Result<Option<FeedbackHistoryRepairProgress>> {
    let mut rows = conn
        .query(
            "SELECT owner_json, feedback_frontier, feedback_cursor, phase, started_at
             FROM memory_v2_feedback_history_repair_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    else {
        return Ok(None);
    };
    let owner_json = row
        .get::<String>(0)
        .map_err(|error| db_error(OPERATION, error))?;
    if owner_json != owner.json {
        return Err(db_message(
            OPERATION,
            "feedback history repair owner identity does not match progress",
        ));
    }
    let progress = FeedbackHistoryRepairProgress {
        feedback_frontier: row.get(1).map_err(|error| db_error(OPERATION, error))?,
        feedback_cursor: row.get(2).map_err(|error| db_error(OPERATION, error))?,
        phase: row.get(3).map_err(|error| db_error(OPERATION, error))?,
        started_at: row.get(4).map_err(|error| db_error(OPERATION, error))?,
    };
    if progress.feedback_cursor > progress.feedback_frontier {
        return Err(db_message(
            OPERATION,
            "feedback history repair cursor exceeds captured frontier",
        ));
    }
    Ok(Some(progress))
}

async fn advance_feedback_history_repair(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    cursor: i64,
) -> Result<()> {
    let updated_at = now_micros()?;
    let changed = conn
        .execute(
            "UPDATE memory_v2_feedback_history_repair_progress
             SET feedback_cursor = ?1, updated_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5
               AND phase = 'pending' AND feedback_cursor <= ?1",
            params![
                cursor,
                updated_at,
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if changed != 1 {
        return Err(db_message(
            OPERATION,
            "feedback history repair progress was not advanceable",
        ));
    }
    Ok(())
}

async fn complete_feedback_history_repair(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    cursor: i64,
) -> Result<()> {
    let completed_at = now_micros()?;
    let changed = conn
        .execute(
            "UPDATE memory_v2_feedback_history_repair_progress
             SET feedback_cursor = ?1, phase = 'complete',
                 updated_at = ?2, completed_at = ?2
             WHERE owner_kind = ?3 AND project_id = ?4 AND source_store_id = ?5
               AND phase = 'pending' AND feedback_frontier = ?1
               AND feedback_cursor <= ?1",
            params![
                cursor,
                completed_at,
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if changed != 1 {
        return Err(db_message(
            OPERATION,
            "feedback history repair progress was not completable",
        ));
    }
    Ok(())
}
