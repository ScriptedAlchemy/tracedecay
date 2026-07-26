//! Payload/FTS/vector purge, quarantine, and legacy-fact cleanup writers.
//!
//! Split out of the former single-file `writers` module as a pure mechanical
//! move; contents are unchanged.

use tracedecay_domain::{
    FactEventId, FactId, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1,
    PayloadAccessState, SourceStoreId, UtcMicros,
};

#[cfg(test)]
use crate::db::engine;
use crate::db::engine::params;
use crate::errors::Result;
use crate::tracedecay::current_timestamp;

use super::super::types::OwnerKey;
use super::super::{
    MemoryV2Executor, OPERATION, canonical_replay, current_fact_state, db_error, db_message,
    load_legacy_entity_ids, optional_i64, optional_string, row_exists,
};
#[cfg(test)]
use super::super::{
    begin, finish_transaction, owner_key, validate_scope, validate_v1_compatibility_source,
};
use super::lineage::insert_event;

/// Purges payload, FTS, and vector material for one exact owner/store/fact.
/// Immutable identity, assertion headers, mapping, and typed lineage remain.
///
/// Standalone transaction wrapper retained for owner-bound purge tests; the
/// production purge path drives `purge_memory_v2_fact_inner` inside a
/// caller-owned authority transaction.
#[cfg(test)]
pub(in crate::db::memory_v2) async fn purge_memory_v2_fact(
    conn: &engine::Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: &FactEventId,
    occurred_at: UtcMicros,
) -> Result<bool> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    fact_id
        .validate()
        .map_err(|_| db_message("memory_v2_purge", "fact identity is invalid"))?;
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let owner_key = owner_key(owner)?;
    let transaction = begin(conn, "memory_v2_purge").await?;
    let result = purge_memory_v2_fact_inner(
        &transaction,
        owner,
        &owner_key,
        source_store_id,
        fact_id,
        Some(expected_last_event_id),
        occurred_at,
    )
    .await;
    let purged = finish_transaction(transaction, result, "memory_v2_purge").await?;
    if purged {
        conn.execute_batch("PRAGMA incremental_vacuum(64)")
            .await
            .map_err(|error| db_error("memory_v2_purge", error))?;
    }
    Ok(purged)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db::memory_v2) async fn quarantine_fact(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    legacy_fact_id: i64,
    reason: &'static str,
    recorded_at: i64,
) -> Result<()> {
    insert_quarantine(
        conn,
        owner_key,
        source_store_id,
        "memory_facts",
        legacy_fact_id,
        reason,
        recorded_at,
    )
    .await?;
    purge_payload_rows(conn, owner_key, fact_id).await?;
    let previous = current_fact_state(conn, owner_key, fact_id).await?.access;
    let event_id =
        if previous != PayloadAccessState::Deleted && previous != PayloadAccessState::Quarantined {
            let event = FactLineageEventV1::new(
                fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::PayloadAccessChanged {
                    previous,
                    current: PayloadAccessState::Quarantined,
                },
                UtcMicros(recorded_at),
                None,
            )
            .map_err(|_| db_message(OPERATION, "typed quarantine event construction failed"))?;
            insert_event(conn, owner_key, &event, recorded_at).await?;
            Some(event.event_id().clone())
        } else {
            None
        };
    // A failed import is evidence, not authorization to destroy its only raw
    // payload. Keep the legacy row intact so an operator can repair or export
    // it; only the rejected V2 projection is made inaccessible.
    if let Some(event_id) = event_id {
        conn.execute(
            "UPDATE memory_v2_current_facts SET
                payload_access = 'quarantined', active_assertion_id = NULL,
                last_event_id = ?1, updated_at = MAX(updated_at, ?2)
             WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
            params![
                event_id.as_str(),
                recorded_at,
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

pub(in crate::db::memory_v2) async fn purge_memory_v2_fact_inner(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: Option<&FactEventId>,
    occurred_at: UtcMicros,
) -> Result<bool> {
    let legacy_fact_id = optional_i64(
        conn,
        "SELECT legacy_fact_id FROM memory_v2_legacy_map
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
           AND source_store_id = ?4",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await?;
    let fact_exists = row_exists(
        conn,
        "SELECT 1 FROM memory_v2_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await?;
    if !fact_exists {
        return Ok(false);
    }
    if legacy_fact_id.is_none()
        && row_exists(
            conn,
            "SELECT 1 FROM memory_v2_legacy_map
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await?
    {
        return Ok(false);
    }
    if expected_last_event_id.is_some()
        && legacy_fact_id.is_some()
        && !row_exists(
            conn,
            "SELECT 1 FROM memory_v2_backfill_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
               AND phase = 'cutover_complete'",
            params![
                owner_key.kind,
                owner_key.project_id.as_str(),
                source_store_id.as_str()
            ],
        )
        .await?
    {
        return Err(db_message(
            "memory_v2_purge",
            "legacy payload purge requires a verified completed backfill",
        ));
    }
    let current = current_fact_state(conn, owner_key, fact_id).await?;
    if expected_last_event_id.is_some_and(|expected| expected != &current.last_event_id) {
        return Err(db_message(
            "memory_v2_purge",
            "fact lineage changed before payload purge",
        ));
    }
    if current.access == PayloadAccessState::Deleted {
        return Ok(false);
    }
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: current.access,
            current: PayloadAccessState::Deleted,
        },
        occurred_at,
        None,
    )
    .map_err(|_| {
        db_message(
            "memory_v2_purge",
            "typed deletion event construction failed",
        )
    })?;
    insert_event(conn, owner_key, &event, occurred_at.0).await?;
    purge_payload_rows(conn, owner_key, fact_id).await?;
    // Backfill replays historical tombstones before snapshot facts. It must
    // never erase the source snapshot while migration is still proving
    // completeness. Only an explicit live purge carrying a CAS event may
    // remove the compatibility row.
    if expected_last_event_id.is_some()
        && let Some(legacy_fact_id) = legacy_fact_id
    {
        purge_legacy_fact(conn, legacy_fact_id).await?;
    }
    conn.execute(
        "UPDATE memory_v2_current_facts SET
            payload_access = 'deleted', active_assertion_id = NULL,
            last_event_id = ?1, updated_at = MAX(updated_at, ?2)
         WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
        params![
            event.event_id().as_str(),
            occurred_at.0,
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    Ok(true)
}

pub(in crate::db::memory_v2) async fn purge_payload_rows(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> Result<()> {
    // Backfill quarantine reaches this helper without passing through the
    // public purge entrypoint, so set the deletion policy at every destructive
    // payload path.
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "UPDATE memory_v2_feedback_history
         SET source = NULL, note = NULL,
             details_availability = CASE
                 WHEN details_availability = 'available' THEN 'legacy_redacted'
                 ELSE details_availability
             END
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "DELETE FROM memory_v2_assertion_vectors
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "DELETE FROM memory_v2_assertion_payloads
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    Ok(())
}

async fn purge_legacy_fact(conn: &impl MemoryV2Executor, legacy_fact_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_bank_dirty(bank_name, updated_at)
         SELECT bank_name, ?1 FROM memory_banks
         WHERE 1
         ON CONFLICT(bank_name) DO UPDATE SET updated_at = excluded.updated_at",
        params![current_timestamp()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute("DELETE FROM memory_banks", ())
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let entity_ids = load_legacy_entity_ids(conn, legacy_fact_id, "memory_v2_purge").await?;
    conn.execute(
        "DELETE FROM memory_facts WHERE fact_id = ?1",
        params![legacy_fact_id],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    for entity_id in entity_ids {
        conn.execute(
            "DELETE FROM memory_entities
             WHERE entity_id = ?1
               AND NOT EXISTS(
                   SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
               )",
            params![entity_id],
        )
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    }
    Ok(())
}

pub(in crate::db::memory_v2) async fn insert_quarantine(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    source_table: &'static str,
    source_row_id: i64,
    reason_code: &'static str,
    recorded_at: i64,
) -> Result<()> {
    if let Some(existing) = optional_string(
        conn,
        "SELECT reason_code FROM memory_v2_legacy_quarantine
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND source_table = ?4 AND source_row_id = ?5",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            source_table,
            source_row_id
        ],
    )
    .await?
    {
        return canonical_replay(existing, reason_code, "legacy quarantine record");
    }
    conn.execute(
        "INSERT INTO memory_v2_legacy_quarantine(
            owner_kind, project_id, source_store_id, source_table,
            source_row_id, reason_code, recorded_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            source_table,
            source_row_id,
            reason_code,
            recorded_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}
