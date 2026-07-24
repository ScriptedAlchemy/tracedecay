use tracedecay_domain::{
    Confidence, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, SourceStoreId,
};

use crate::db::engine::params;
use crate::errors::Result;

use super::super::schema::v22_feedback_history_schema_installed;
use super::super::types::{LegacyFeedback, MemoryV2BackfillBatchOutcome, OwnerKey, Progress};
use super::super::writers::{
    insert_event, insert_feedback_history, insert_legacy_feedback_event_mapping, insert_quarantine,
    legacy_feedback_mapping_can_be_recorded, update_current,
};
use super::super::{
    MemoryV2Executor, OPERATION, db_error, db_message, sanitize_legacy_feedback_details,
    seconds_to_micros, update_cursor, update_phase,
};
use super::facts::ensure_legacy_identity;

async fn load_legacy_feedback_batch(
    conn: &impl MemoryV2Executor,
    cursor: i64,
    frontier: i64,
    limit: i64,
) -> Result<Vec<LegacyFeedback>> {
    let mut rows = conn
        .query(
            "SELECT event_id, fact_id, action, old_trust, new_trust, created_at, source, note
             FROM memory_feedback_events
             WHERE event_id > ?1 AND event_id <= ?2
             ORDER BY event_id LIMIT ?3",
            params![cursor, frontier, limit],
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

pub(in crate::db) async fn backfill_feedback_batch(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    progress: &Progress,
    limit: i64,
) -> Result<MemoryV2BackfillBatchOutcome> {
    let write_v22_feedback_history = v22_feedback_history_schema_installed(conn).await?;
    let batch = load_legacy_feedback_batch(
        conn,
        progress.feedback_cursor,
        progress.feedback_frontier,
        limit,
    )
    .await?;
    if batch.is_empty() {
        update_phase(conn, owner_key, source_store_id, "oplog").await?;
        return Ok(MemoryV2BackfillBatchOutcome::Advanced { processed: 0 });
    }
    for item in &batch {
        let action = match item.action.as_str() {
            "helpful" => "helpful",
            "unhelpful" => "unhelpful",
            _ => {
                insert_quarantine(
                    conn,
                    owner_key,
                    source_store_id,
                    "memory_feedback_events",
                    item.event_id,
                    "unknown_feedback_action",
                    progress.started_at,
                )
                .await?;
                continue;
            }
        };
        let fact_id = ensure_legacy_identity(
            conn,
            owner,
            owner_key,
            source_store_id,
            item.fact_id,
            progress.started_at,
        )
        .await?;
        let previous = Confidence::new(item.old_trust);
        let current = Confidence::new(item.new_trust);
        let occurred_at = seconds_to_micros(item.created_at);
        let (Ok(previous), Ok(current), Some(occurred_at)) = (previous, current, occurred_at)
        else {
            insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "invalid_feedback_contract",
                progress.started_at,
            )
            .await?;
            continue;
        };
        if previous == current {
            insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "non_transition_feedback",
                progress.started_at,
            )
            .await?;
            continue;
        }
        if (action == "helpful" && current <= previous)
            || (action == "unhelpful" && current >= previous)
        {
            insert_quarantine(
                conn,
                owner_key,
                source_store_id,
                "memory_feedback_events",
                item.event_id,
                "feedback_action_direction_mismatch",
                progress.started_at,
            )
            .await?;
            continue;
        }
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous,
                current,
                evidence_ids: Vec::new(),
            },
            occurred_at,
            None,
        )
        .map_err(|_| db_message(OPERATION, "typed feedback event construction failed"))?;
        if write_v22_feedback_history
            && !legacy_feedback_mapping_can_be_recorded(
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
            continue;
        }
        insert_event(conn, owner_key, &event, progress.started_at).await?;
        if write_v22_feedback_history {
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
            .await?;
        }
        update_current(
            conn,
            owner_key,
            &fact_id,
            None,
            Some(current.as_f64()),
            event.event_id(),
            occurred_at.0,
        )
        .await?;
    }
    let cursor = batch
        .last()
        .map_or(progress.feedback_cursor, |item| item.event_id);
    update_cursor(conn, owner_key, source_store_id, "feedback_cursor", cursor).await?;
    Ok(MemoryV2BackfillBatchOutcome::Advanced {
        processed: batch.len(),
    })
}
