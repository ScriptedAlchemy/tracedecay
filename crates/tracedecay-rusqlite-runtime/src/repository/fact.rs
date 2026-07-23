use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use serde::Serialize;
use tracedecay_domain::{
    Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactEventId, FactId,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, PayloadAccessState,
    RetrievalAnchorRecordV2, UtcMicros,
};
use tracedecay_store::{
    FactCurrentQuery, FactLineageQuery, FactReadOperationV1, FactReadResultV1, FactWriteBatch,
    StoredFactV1,
};

use super::support::{decode, encode, invalid, usize_to_i64};

#[derive(Clone, Default)]
pub struct FactExecutor;

impl FactExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        batch: &FactWriteBatch,
    ) -> rusqlite::Result<()> {
        let owner = OwnerColumns::new(batch.owner())?;
        let actual_last = current_last_event(savepoint, &owner, batch.fact_id())?;
        if actual_last.as_ref() == batch.events().last().map(FactLineageEventV1::event_id)
            && batch
                .events()
                .iter()
                .all(|event| event_matches(savepoint, &owner, event).unwrap_or(false))
        {
            return Ok(());
        }
        if actual_last.as_ref() != batch.expected_last_event_id() {
            return Err(invalid("fact lineage last-event conflict"));
        }

        ensure_fact(savepoint, &owner, batch)?;
        for anchor_id in batch.referenced_anchor_ids() {
            let exists = savepoint.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM retrieval_anchors
                    WHERE anchor_id = ?1 AND owner_json = ?2
                 )",
                params![anchor_id.as_str(), owner.json],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(invalid("fact references an unavailable retrieval anchor"));
            }
        }
        for anchor in batch.new_anchors() {
            insert_anchor(savepoint, &owner, anchor)?;
        }
        if let Some(assertion) = batch.assertion() {
            insert_assertion(savepoint, &owner, assertion)?;
        }
        if let Some(mapping) = batch.legacy_mapping() {
            savepoint.execute(
                "INSERT INTO memory_v2_legacy_map (
                    owner_kind, project_id, owner_json, source_store_id,
                    legacy_fact_id, fact_id, mapping_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    owner.kind,
                    owner.project_id,
                    owner.json,
                    mapping.source_store_id().as_str(),
                    mapping.legacy_fact_id(),
                    mapping.fact_id().as_str(),
                    encode(mapping)?,
                ],
            )?;
        }
        for event in batch.events() {
            savepoint.execute(
                "INSERT INTO memory_v2_lineage_events (
                    event_id, fact_id, owner_kind, project_id,
                    event_json, occurred_at, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id().as_str(),
                    event.fact_id().as_str(),
                    owner.kind,
                    owner.project_id,
                    encode(event)?,
                    event.occurred_at().0,
                    event.occurred_at().0,
                ],
            )?;
        }
        publish_projection(savepoint, &owner, batch)
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &FactReadOperationV1,
    ) -> rusqlite::Result<FactReadResultV1> {
        match operation {
            FactReadOperationV1::Current(query) => {
                read_current(snapshot, query).map(|fact| FactReadResultV1::Current(Box::new(fact)))
            }
            FactReadOperationV1::Lineage(query) => {
                read_lineage(snapshot, query).map(FactReadResultV1::Lineage)
            }
        }
    }
}

struct OwnerColumns {
    kind: &'static str,
    project_id: String,
    json: String,
}

impl OwnerColumns {
    fn new(owner: &FactOwnerV1) -> rusqlite::Result<Self> {
        let (kind, project_id) = match owner {
            FactOwnerV1::Profile => ("profile", String::new()),
            FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
        };
        Ok(Self {
            kind,
            project_id,
            json: encode(owner)?,
        })
    }
}

#[derive(Serialize)]
struct StoredAssertionHeaderV1<'a> {
    assertion_id: &'a FactAssertionId,
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    kind: &'a FactAssertionKindV1,
    payload_reference: &'a tracedecay_domain::PayloadReferenceV1,
    evidence: &'a [tracedecay_domain::FactEvidenceRefV1],
    asserted_at: UtcMicros,
    actor_id: Option<&'a tracedecay_domain::ActorId>,
}

fn current_last_event(
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

fn ensure_fact(
    savepoint: &Savepoint<'_>,
    owner: &OwnerColumns,
    batch: &FactWriteBatch,
) -> rusqlite::Result<()> {
    let exists = savepoint.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_v2_facts WHERE fact_id = ?1)",
        [batch.fact_id().as_str()],
        |row| row.get::<_, bool>(0),
    )?;
    if exists {
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

fn insert_anchor(
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

fn insert_assertion(
    savepoint: &Savepoint<'_>,
    owner: &OwnerColumns,
    assertion: &FactAssertionV1,
) -> rusqlite::Result<()> {
    let payload_reference = assertion.payload().payload_reference().map_err(invalid)?;
    let header = encode(&StoredAssertionHeaderV1 {
        assertion_id: assertion.assertion_id(),
        fact_id: assertion.fact_id(),
        owner: assertion.owner(),
        kind: assertion.kind(),
        payload_reference: &payload_reference,
        evidence: assertion.evidence(),
        asserted_at: assertion.asserted_at(),
        actor_id: assertion.actor_id(),
    })?;
    savepoint.execute(
        "INSERT INTO memory_v2_assertions (
            assertion_id, fact_id, owner_kind, project_id, owner_json,
            assertion_header_json, kind_json, payload_reference_json,
            receipt_json, asserted_at, actor_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id,
            owner.json,
            header,
            encode(assertion.kind())?,
            encode(&payload_reference)?,
            encode(assertion.payload().receipt())?,
            assertion.asserted_at().0,
            assertion.actor_id().map(|actor| actor.as_str()),
        ],
    )?;
    for (ordinal, superseded) in superseded_assertions(assertion.kind()).iter().enumerate() {
        savepoint.execute(
            "INSERT INTO memory_v2_assertion_supersession (
                assertion_id, fact_id, owner_kind, project_id,
                superseded_assertion_id, ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id,
                superseded.as_str(),
                usize_to_i64(ordinal, "assertion supersession ordinal")?,
            ],
        )?;
    }
    savepoint.execute(
        "INSERT INTO memory_v2_assertion_payloads (
            assertion_id, fact_id, owner_kind, project_id, payload_json, content
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id,
            encode(assertion.payload())?,
            assertion.payload().content(),
        ],
    )?;
    for (ordinal, evidence) in assertion.evidence().iter().enumerate() {
        savepoint.execute(
            "INSERT OR IGNORE INTO memory_v2_evidence (
                evidence_id, fact_id, owner_kind, project_id,
                owner_json, anchor_id, evidence_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                evidence.evidence_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id,
                owner.json,
                evidence.anchor_id().as_str(),
                encode(evidence)?,
            ],
        )?;
        savepoint.execute(
            "INSERT INTO memory_v2_assertion_evidence (
                assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                evidence.evidence_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id,
                usize_to_i64(ordinal, "assertion evidence ordinal")?,
            ],
        )?;
    }
    Ok(())
}

fn superseded_assertions(kind: &FactAssertionKindV1) -> Vec<&FactAssertionId> {
    match kind {
        FactAssertionKindV1::Correction { supersedes } => vec![supersedes],
        FactAssertionKindV1::Merge { supersedes } => supersedes.iter().collect(),
        FactAssertionKindV1::Initial | FactAssertionKindV1::LegacyImport => Vec::new(),
    }
}

fn publish_projection(
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
            FactLineageEventKindV1::TrustChanged { current, .. } => trust = current.as_f64(),
            FactLineageEventKindV1::PayloadAccessChanged { current, .. } => {
                access = encode(current)?.trim_matches('"').to_owned();
            }
            FactLineageEventKindV1::Curated { .. }
            | FactLineageEventKindV1::LegacyImported { .. } => {}
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

fn event_matches(
    connection: &rusqlite::Connection,
    owner: &OwnerColumns,
    event: &FactLineageEventV1,
) -> rusqlite::Result<bool> {
    let stored = connection
        .query_row(
            "SELECT fact_id, owner_kind, project_id, event_json, occurred_at
             FROM memory_v2_lineage_events WHERE event_id = ?1",
            [event.event_id().as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    Ok(stored.is_some_and(|stored| {
        stored.0 == event.fact_id().as_str()
            && stored.1 == owner.kind
            && stored.2 == owner.project_id
            && stored.3 == encode(event).unwrap_or_default()
            && stored.4 == event.occurred_at().0
    }))
}

fn read_current(
    connection: &rusqlite::Connection,
    query: &FactCurrentQuery,
) -> rusqlite::Result<Option<StoredFactV1>> {
    let owner = OwnerColumns::new(query.owner())?;
    let row = connection
        .query_row(
            "SELECT facts.owner_json, current.payload_access, current.trust_score,
                    current.active_assertion_id, current.last_event_id, current.updated_at,
                    payload.payload_json, legacy.mapping_json
             FROM memory_v2_current_facts AS current
             JOIN memory_v2_facts AS facts
               USING(fact_id, owner_kind, project_id)
             LEFT JOIN memory_v2_assertion_payloads AS payload
               ON payload.assertion_id = current.active_assertion_id
              AND payload.fact_id = current.fact_id
              AND payload.owner_kind = current.owner_kind
              AND payload.project_id = current.project_id
             LEFT JOIN memory_v2_legacy_map AS legacy
               USING(fact_id, owner_kind, project_id)
             WHERE current.fact_id = ?1
               AND current.owner_kind = ?2
               AND current.project_id = ?3",
            params![query.fact_id().as_str(), owner.kind, owner.project_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        owner_json,
        access,
        trust,
        active_assertion,
        last_event,
        updated_at,
        payload,
        legacy,
    )) = row
    else {
        return Ok(None);
    };
    let owner_value: FactOwnerV1 = decode(owner_json)?;
    if &owner_value != query.owner() {
        return Err(invalid("stored fact owner does not match read authority"));
    }
    let access: PayloadAccessState = decode(format!("\"{access}\""))?;
    let payload = if access == PayloadAccessState::Eligible {
        payload.map(decode::<FactPayloadV1>).transpose()?
    } else {
        None
    };
    let active_assertion =
        active_assertion.ok_or_else(|| invalid("current fact has no active assertion"))?;
    StoredFactV1::new(
        query.fact_id().clone(),
        owner_value,
        payload,
        access,
        Confidence::new(trust.unwrap_or(0.5)).map_err(invalid)?,
        FactAssertionId::new(active_assertion).map_err(invalid)?,
        FactEventId::new(last_event).map_err(invalid)?,
        legacy
            .map(decode::<tracedecay_domain::LegacyFactMappingV1>)
            .transpose()?,
        UtcMicros(updated_at),
    )
    .map(Some)
    .map_err(invalid)
}

fn read_lineage(
    connection: &rusqlite::Connection,
    query: &FactLineageQuery,
) -> rusqlite::Result<Vec<FactLineageEventV1>> {
    let owner = OwnerColumns::new(query.owner())?;
    let limit = usize_to_i64(query.limit(), "fact lineage limit")?;
    let mut events = Vec::new();
    if let Some(after) = query.after() {
        let mut statement = connection.prepare(
            "SELECT event_json FROM memory_v2_lineage_events
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
               AND (occurred_at > ?4 OR (occurred_at = ?4 AND event_id > ?5))
             ORDER BY occurred_at, event_id LIMIT ?6",
        )?;
        let rows = statement.query_map(
            params![
                query.fact_id().as_str(),
                owner.kind,
                owner.project_id,
                after.occurred_at().0,
                after.event_id().as_str(),
                limit,
            ],
            |row| row.get::<_, String>(0),
        )?;
        for row in rows {
            events.push(decode(row?)?);
        }
    } else {
        let mut statement = connection.prepare(
            "SELECT event_json FROM memory_v2_lineage_events
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
             ORDER BY occurred_at, event_id LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                query.fact_id().as_str(),
                owner.kind,
                owner.project_id,
                limit,
            ],
            |row| row.get::<_, String>(0),
        )?;
        for row in rows {
            events.push(decode(row?)?);
        }
    }
    Ok(events)
}
