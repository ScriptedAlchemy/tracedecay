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
    MemoryV2Executor, current_fact_state, db_error, db_message, load_legacy_entity_ids,
    optional_i64, owner_key, row_exists, validate_scope, validate_v1_compatibility_source,
};
#[cfg(test)]
use super::super::{begin, finish_transaction};
use super::insert_event;

/// A live purge that also reclaims the legacy compatibility row. It carries the
/// lineage CAS expectation and requires a verified completed backfill; the
/// backfill's tombstone-replay intent disappeared with the backfill itself.
#[derive(Clone, Copy)]
pub(in crate::db::memory_v2) struct PurgeIntent<'a> {
    expected_last_event_id: &'a FactEventId,
    actor: Option<&'a ActorId>,
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
        PurgeIntent {
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
        PurgeIntent {
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
    let reclamation = match legacy_fact_id {
        Some(legacy_fact_id) => Some((
            LegacyReclamationAuthorization::authorize(conn, owner_key, source_store_id).await?,
            legacy_fact_id,
        )),
        None => None,
    };
    let current = current_fact_state(conn, owner_key, fact_id).await?;
    if intent.expected_last_event_id != &current.last_event_id {
        return Err(db_message(
            "memory_v2_purge",
            "fact lineage changed before payload purge",
        ));
    }
    if current.access == PayloadAccessState::Deleted {
        return Ok(false);
    }
    let actor = intent.actor.cloned();
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
    // Only an authorized live reclamation — one that minted the
    // `cutover_complete` token above — reaches this destructive step.
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

async fn purge_payload_rows(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> Result<()> {
    // Transactional callers reach this helper without passing through the
    // connection-level purge entrypoint, so set the deletion policy at every
    // destructive payload path.
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
