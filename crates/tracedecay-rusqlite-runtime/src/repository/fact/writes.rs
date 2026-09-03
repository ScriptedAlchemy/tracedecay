//! The fact, anchor, and current-projection rows one fact batch writes.
//!
//! These are the parts of a batch that are not the assertion itself: the fact
//! row it hangs off, the anchors its evidence references, and the current-fact
//! projection its lineage events fold into.

use rusqlite::{OptionalExtension, Savepoint, params};
use tracedecay_domain::{
    FactEventId, FactId, FactIdentityMaterialV1, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, PayloadAccessState, RetrievalAnchorRecordV2,
};
use tracedecay_store::FactWriteBatch;

use super::super::support::{decode, encode, invalid};
use super::OwnerColumns;

pub(super) fn current_last_event(
    connection: &rusqlite::Connection,
    owner: &OwnerColumns,
    fact_id: &FactId,
) -> rusqlite::Result<Option<FactEventId>> {
    connection
        .query_row(
            "SELECT last_event_id FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(FactEventId::new)
        .transpose()
        .map_err(invalid)
}

pub(super) fn ensure_fact(
    savepoint: &Savepoint<'_>,
    owner: &OwnerColumns,
    batch: &FactWriteBatch,
) -> rusqlite::Result<()> {
    let stored = savepoint
        .query_row(
            "SELECT owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((stored_owner, stored_identity)) = stored {
        let stored_owner = decode::<FactOwnerV1>(stored_owner)?;
        let stored_identity = decode::<FactIdentityMaterialV1>(stored_identity)?;
        let derived = FactId::derive(&stored_identity).map_err(invalid)?;
        if &stored_owner != batch.owner()
            || stored_identity.owner() != batch.owner()
            || &derived != batch.fact_id()
            || batch
                .identity_material()
                .is_some_and(|candidate| candidate != &stored_identity)
        {
            return Err(invalid("fact identity collision"));
        }
        return Ok(());
    }
    let identity = batch
        .identity_material()
        .ok_or_else(|| invalid("new fact requires canonical identity material"))?;
    let created_at = batch
        .events()
        .first()
        .map(FactLineageEventV1::occurred_at)
        .ok_or_else(|| invalid("fact batch is empty"))?;
    savepoint.execute(
        "INSERT INTO memory_v2_facts (
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            batch.fact_id().as_str(),
            owner.kind,
            owner.project_id,
            owner.json,
            encode(identity)?,
            created_at.0,
        ],
    )?;
    Ok(())
}

pub(super) fn insert_anchor(
    savepoint: &Savepoint<'_>,
    owner: &OwnerColumns,
    anchor: &RetrievalAnchorRecordV2,
) -> rusqlite::Result<()> {
    let encoded = encode(anchor)?;
    let stored = savepoint
        .query_row(
            "SELECT anchor_json, owner_json FROM retrieval_anchors WHERE anchor_id = ?1",
            [anchor.anchor_id().as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((stored_anchor, stored_owner)) = stored {
        return if stored_anchor == encoded && stored_owner == owner.json {
            Ok(())
        } else {
            Err(invalid("retrieval anchor identity collision"))
        };
    }
    savepoint.execute(
        "INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            anchor.anchor_id().as_str(),
            encoded,
            owner.json,
            anchor.projection_generation().as_str(),
        ],
    )?;
    for alias in anchor.aliases() {
        savepoint.execute(
            "INSERT INTO retrieval_anchor_aliases (
                owner_json, alias_kind, locator_digest, anchor_id
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                owner.json,
                encode(&alias.kind())?,
                encode(alias.locator_digest())?,
                anchor.anchor_id().as_str(),
            ],
        )?;
    }
    Ok(())
}

pub(super) fn publish_projection(
    savepoint: &Savepoint<'_>,
    owner: &OwnerColumns,
    batch: &FactWriteBatch,
) -> rusqlite::Result<()> {
    let existing = savepoint
        .query_row(
            "SELECT payload_access, trust_score, active_assertion_id
             FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![batch.fact_id().as_str(), owner.kind, owner.project_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<f64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let (mut access, mut trust, mut active) = match existing {
        Some((access, trust, active)) => (access, trust.unwrap_or(0.5), active),
        None => ("eligible".to_owned(), 0.5, None),
    };
    for event in batch.events() {
        match event.kind() {
            FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
                active = Some(assertion_id.as_str().to_owned());
            }
            FactLineageEventKindV1::TrustChanged {
                previous, current, ..
            } => {
                if previous.as_f64() != trust {
                    return Err(invalid("fact trust transition is stale"));
                }
                trust = current.as_f64();
            }
            FactLineageEventKindV1::PayloadAccessChanged { previous, current } => {
                let previous = encode(previous)?;
                if previous.trim_matches('"') != access.as_str() {
                    return Err(invalid("fact payload access transition is stale"));
                }
                access = encode(current)?.trim_matches('"').to_owned();
                if matches!(
                    current,
                    PayloadAccessState::Quarantined | PayloadAccessState::Deleted
                ) {
                    active = None;
                }
            }
            FactLineageEventKindV1::Curated { .. } => {}
        }
    }
    let last = batch
        .events()
        .last()
        .ok_or_else(|| invalid("fact batch is empty"))?;
    savepoint.execute(
        "INSERT INTO memory_v2_current_facts (
            fact_id, owner_kind, project_id, payload_access, trust_score,
            active_assertion_id, last_event_id, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(fact_id, owner_kind, project_id) DO UPDATE SET
            payload_access = excluded.payload_access,
            trust_score = excluded.trust_score,
            active_assertion_id = excluded.active_assertion_id,
            last_event_id = excluded.last_event_id,
            updated_at = excluded.updated_at",
        params![
            batch.fact_id().as_str(),
            owner.kind,
            owner.project_id,
            access,
            trust,
            active,
            last.event_id().as_str(),
            last.occurred_at().0,
        ],
    )?;
    Ok(())
}
