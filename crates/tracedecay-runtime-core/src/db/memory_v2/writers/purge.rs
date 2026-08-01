//! Payload/FTS/vector purge, quarantine, and legacy-fact cleanup writers.

use serde::Serialize;
use tracedecay_domain::{
    ActorId, FactEventId, FactId, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1,
    PayloadAccessState, SourceStoreId, UtcMicros,
};

use crate::db::engine::params;
use crate::errors::Result;
use crate::tracedecay::current_timestamp;

use self::legacy_reclamation::LegacyReclamationAuthorization;
use super::super::types::OwnerKey;
use super::super::{
    MemoryV2Executor, OPERATION, canonical_replay, current_fact_state, db_error, db_message,
    load_legacy_entity_ids, optional_i64, optional_string, owner_key, row_exists, validate_scope,
    validate_v1_compatibility_source,
};
#[cfg(test)]
use super::super::{begin, finish_transaction};
use super::lineage::insert_event;

/// Why a fact's payload is being purged. The variant, not an incidental
/// `Option`, decides whether the legacy compatibility row may be reclaimed.
#[derive(Clone, Copy)]
pub(in crate::db::memory_v2) enum PurgeIntent<'a> {
    /// Backfill replaying a historical legacy tombstone. Backfill has not yet
    /// proven the payload was copied, so the legacy row is always retained.
    ReplayLegacyTombstone,
    /// Live purge that also reclaims the legacy compatibility row. Carries the
    /// lineage CAS expectation and requires a verified completed backfill.
    ReclaimLegacyPayload {
        expected_last_event_id: &'a FactEventId,
        actor: Option<&'a ActorId>,
    },
}

/// Type-level authority to destroy legacy compatibility payloads.
///
/// The struct's only field is private to this module, so
/// [`LegacyReclamationAuthorization::authorize`] is the sole way to obtain a
/// value, and it fails unless the backfill for that exact owner/store reached
/// `cutover_complete`. Legacy reclamation takes the token by reference, so a
/// purge path that skips the cutover gate does not compile.
mod legacy_reclamation {
    use tracedecay_domain::SourceStoreId;

    use super::{MemoryV2Executor, OwnerKey, Result};

    pub(super) struct LegacyReclamationAuthorization {
        _verified: (),
    }

    impl LegacyReclamationAuthorization {
        pub(super) async fn authorize(
            conn: &impl MemoryV2Executor,
            owner_key: &OwnerKey,
            source_store_id: &SourceStoreId,
        ) -> Result<Self> {
            super::super::super::cutover::verify_memory_v2_cutover_complete(
                conn,
                owner_key,
                source_store_id,
            )
            .await?;
            Ok(Self { _verified: () })
        }
    }
}

/// Observable record of one live legacy-payload purge, so a caller can report
/// exactly what a destructive reclamation did rather than a bare boolean.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct MemoryV2LegacyPurgeReceipt {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    fact_id: FactId,
    expected_last_event_id: FactEventId,
    occurred_at: UtcMicros,
    payload_purged: bool,
}

impl MemoryV2LegacyPurgeReceipt {
    pub(crate) fn payload_purged(&self) -> bool {
        self.payload_purged
    }
}

/// Purges payload, FTS, and vector material for one exact owner/store/fact.
/// Immutable identity, assertion headers, mapping, and typed lineage remain.
///
/// This is the single live purge chokepoint: it opens the authority
/// transaction, drives `purge_memory_v2_fact_inner` with a CAS expectation, and
/// therefore always mints the `cutover_complete` authorization before any
/// legacy row can be reclaimed. Production and tests share this exact code so
/// the gate cannot be proven only in a test build.
#[cfg(test)]
pub(in crate::db) async fn purge_memory_v2_fact(
    conn: &crate::db::engine::Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: &FactEventId,
    occurred_at: UtcMicros,
) -> Result<MemoryV2LegacyPurgeReceipt> {
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
        PurgeIntent::ReclaimLegacyPayload {
            expected_last_event_id,
            actor: None,
        },
        occurred_at,
    )
    .await;
    let payload_purged = finish_transaction(transaction, result, "memory_v2_purge").await?;
    Ok(MemoryV2LegacyPurgeReceipt {
        owner: owner.clone(),
        source_store_id: source_store_id.clone(),
        fact_id: fact_id.clone(),
        expected_last_event_id: expected_last_event_id.clone(),
        occurred_at,
        payload_purged,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn purge_memory_v2_fact_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: &FactEventId,
    actor: Option<&ActorId>,
    occurred_at: UtcMicros,
) -> Result<MemoryV2LegacyPurgeReceipt> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    fact_id
        .validate()
        .map_err(|_| db_message("memory_v2_purge", "fact identity is invalid"))?;
    let owner_key = owner_key(owner)?;
    let payload_purged = purge_memory_v2_fact_inner(
        conn,
        owner,
        &owner_key,
        source_store_id,
        fact_id,
        PurgeIntent::ReclaimLegacyPayload {
            expected_last_event_id,
            actor,
        },
        occurred_at,
    )
    .await?;
    Ok(MemoryV2LegacyPurgeReceipt {
        owner: owner.clone(),
        source_store_id: source_store_id.clone(),
        fact_id: fact_id.clone(),
        expected_last_event_id: expected_last_event_id.clone(),
        occurred_at,
        payload_purged,
    })
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
    intent: PurgeIntent<'_>,
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
    // Reclaiming the legacy row is the only step here that can destroy the last
    // surviving copy of a payload. Mint its authorization before any mutation
    // so the cutover gate is proven up front and cannot be routed around.
    let reclamation = match (intent, legacy_fact_id) {
        (PurgeIntent::ReclaimLegacyPayload { .. }, Some(legacy_fact_id)) => Some((
            LegacyReclamationAuthorization::authorize(conn, owner_key, source_store_id).await?,
            legacy_fact_id,
        )),
        _ => None,
    };
    let current = current_fact_state(conn, owner_key, fact_id).await?;
    if let PurgeIntent::ReclaimLegacyPayload {
        expected_last_event_id,
        ..
    } = intent
        && expected_last_event_id != &current.last_event_id
    {
        return Err(db_message(
            "memory_v2_purge",
            "fact lineage changed before payload purge",
        ));
    }
    if current.access == PayloadAccessState::Deleted {
        return Ok(false);
    }
    let actor = match intent {
        PurgeIntent::ReplayLegacyTombstone => None,
        PurgeIntent::ReclaimLegacyPayload { actor, .. } => actor.cloned(),
    };
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: current.access,
            current: PayloadAccessState::Deleted,
        },
        occurred_at,
        actor,
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
    // completeness, so only an authorized live reclamation reaches this.
    if let Some((authorization, legacy_fact_id)) = &reclamation {
        purge_legacy_fact(conn, *legacy_fact_id, authorization).await?;
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

/// Destroys the legacy compatibility row backing one V2 fact. The
/// authorization argument is the type-level cutover proof; it exists so this
/// function is unreachable without passing the `cutover_complete` gate.
async fn purge_legacy_fact(
    conn: &impl MemoryV2Executor,
    legacy_fact_id: i64,
    _authorization: &LegacyReclamationAuthorization,
) -> Result<()> {
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
