use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use serde::Serialize;
use tracedecay_domain::{
    Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactEventId, FactId,
    FactIdentityMaterialV1, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    PayloadAccessState, RetrievalAnchorRecordV2, UtcMicros,
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

fn assertion_header_json(assertion: &FactAssertionV1) -> rusqlite::Result<String> {
    let payload_reference = assertion.payload().payload_reference().map_err(invalid)?;
    encode(&StoredAssertionHeaderV1 {
        assertion_id: assertion.assertion_id(),
        fact_id: assertion.fact_id(),
        owner: assertion.owner(),
        kind: assertion.kind(),
        payload_reference: &payload_reference,
        evidence: assertion.evidence(),
        asserted_at: assertion.asserted_at(),
        actor_id: assertion.actor_id(),
    })
}

fn insert_assertion(
    savepoint: &Savepoint<'_>,
    owner: &OwnerColumns,
    assertion: &FactAssertionV1,
) -> rusqlite::Result<()> {
    if assertion_exists(savepoint, assertion.assertion_id())? {
        return if assertion_matches(savepoint, owner, assertion)? {
            Ok(())
        } else {
            Err(invalid("assertion identity collision"))
        };
    }
    let payload_reference = assertion.payload().payload_reference().map_err(invalid)?;
    let header = assertion_header_json(assertion)?;
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
        let evidence_json = encode(evidence)?;
        let changed = savepoint.execute(
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
                evidence_json.as_str(),
            ],
        )?;
        if changed == 0 {
            let stored = savepoint
                .query_row(
                    "SELECT evidence_json, owner_json, anchor_id
                     FROM memory_v2_evidence
                     WHERE evidence_id = ?1 AND fact_id = ?2
                       AND owner_kind = ?3 AND project_id = ?4",
                    params![
                        evidence.evidence_id().as_str(),
                        assertion.fact_id().as_str(),
                        owner.kind,
                        owner.project_id,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((stored_json, stored_owner, stored_anchor)) = stored else {
                return Err(invalid("evidence insert disappeared"));
            };
            if stored_json != evidence_json
                || stored_owner != owner.json
                || stored_anchor != evidence.anchor_id().as_str()
            {
                return Err(invalid("evidence identity collision"));
            }
        }
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

fn assertion_exists(
    connection: &rusqlite::Connection,
    assertion_id: &FactAssertionId,
) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM memory_v2_assertions WHERE assertion_id = ?1
         )",
        [assertion_id.as_str()],
        |row| row.get::<_, bool>(0),
    )
}

/// Compare a stored assertion against the one being written across every
/// column the write path persists: the assertion header row, its supersession
/// list, its payload, and its ordered evidence.
///
/// This mirrors the root commit engine so an exact replay is idempotent and a
/// reused assertion id with different content is a collision, rather than a raw
/// primary-key violation from the driver.
fn assertion_matches(
    connection: &rusqlite::Connection,
    owner: &OwnerColumns,
    assertion: &FactAssertionV1,
) -> rusqlite::Result<bool> {
    let stored = connection
        .query_row(
            "SELECT fact_id, owner_kind, project_id, owner_json,
                    assertion_header_json, kind_json, payload_reference_json,
                    receipt_json, asserted_at, actor_id
             FROM memory_v2_assertions WHERE assertion_id = ?1",
            [assertion.assertion_id().as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        stored_fact_id,
        stored_owner_kind,
        stored_project_id,
        stored_owner_json,
        stored_header,
        stored_kind,
        stored_payload_reference,
        stored_receipt,
        stored_asserted_at,
        stored_actor,
    )) = stored
    else {
        return Ok(false);
    };
    let payload_reference = assertion.payload().payload_reference().map_err(invalid)?;
    if stored_fact_id != assertion.fact_id().as_str()
        || stored_owner_kind != owner.kind
        || stored_project_id != owner.project_id
        || stored_owner_json != owner.json
        || stored_header != assertion_header_json(assertion)?
        || stored_kind != encode(assertion.kind())?
        || stored_payload_reference != encode(&payload_reference)?
        || stored_receipt != encode(assertion.payload().receipt())?
        || stored_asserted_at != assertion.asserted_at().0
        || stored_actor.as_deref() != assertion.actor_id().map(|actor| actor.as_str())
    {
        return Ok(false);
    }

    let mut supersession = connection.prepare(
        "SELECT superseded_assertion_id FROM memory_v2_assertion_supersession
         WHERE assertion_id = ?1 AND fact_id = ?2
           AND owner_kind = ?3 AND project_id = ?4 ORDER BY ordinal",
    )?;
    let rows = supersession.query_map(
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id,
        ],
        |row| row.get::<_, String>(0),
    )?;
    let mut stored_supersession = Vec::new();
    for row in rows {
        stored_supersession.push(row?);
    }
    let expected_supersession = superseded_assertions(assertion.kind())
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect::<Vec<_>>();
    if stored_supersession != expected_supersession {
        return Ok(false);
    }

    let stored_payload = connection
        .query_row(
            "SELECT payload_json, content FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let payload_matches = match stored_payload {
        Some((payload_json, content)) => {
            payload_json == encode(assertion.payload())?
                && content == assertion.payload().content()
        }
        None => payload_is_purged_projection(connection, owner, assertion.fact_id())?,
    };
    if !payload_matches {
        return Ok(false);
    }

    let mut evidence = connection.prepare(
        "SELECT assertion_evidence.evidence_id, evidence.evidence_json,
                evidence.owner_json, evidence.anchor_id
         FROM memory_v2_assertion_evidence AS assertion_evidence
         JOIN memory_v2_evidence AS evidence
           ON evidence.evidence_id = assertion_evidence.evidence_id
          AND evidence.fact_id = assertion_evidence.fact_id
          AND evidence.owner_kind = assertion_evidence.owner_kind
          AND evidence.project_id = assertion_evidence.project_id
         WHERE assertion_evidence.assertion_id = ?1
           AND assertion_evidence.fact_id = ?2
           AND assertion_evidence.owner_kind = ?3
           AND assertion_evidence.project_id = ?4
         ORDER BY assertion_evidence.ordinal",
    )?;
    let rows = evidence.query_map(
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    let mut stored_evidence = Vec::new();
    for row in rows {
        stored_evidence.push(row?);
    }
    let expected_evidence = assertion
        .evidence()
        .iter()
        .map(|evidence| {
            Ok((
                evidence.evidence_id().as_str().to_owned(),
                encode(evidence)?,
                owner.json.clone(),
                evidence.anchor_id().as_str().to_owned(),
            ))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(stored_evidence == expected_evidence)
}

/// A missing payload row is only consistent with a purged projection, matching
/// the root engine's allowance for `Quarantined` and `Deleted` access states.
fn payload_is_purged_projection(
    connection: &rusqlite::Connection,
    owner: &OwnerColumns,
    fact_id: &FactId,
) -> rusqlite::Result<bool> {
    let access = connection
        .query_row(
            "SELECT current.payload_access
             FROM memory_v2_current_facts AS current
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current.fact_id
              AND facts.owner_kind = current.owner_kind
              AND facts.project_id = current.project_id
             WHERE current.fact_id = ?1
               AND current.owner_kind = ?2
               AND current.project_id = ?3
               AND facts.owner_json = ?4",
            params![fact_id.as_str(), owner.kind, owner.project_id, owner.json],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(access) = access else {
        return Ok(false);
    };
    Ok(matches!(
        decode::<PayloadAccessState>(format!("\"{access}\""))?,
        PayloadAccessState::Quarantined | PayloadAccessState::Deleted
    ))
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
    let Some(active_assertion) = active_assertion else {
        return Ok(None);
    };
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
    let mut events: Vec<FactLineageEventV1> = Vec::new();
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
    if events
        .iter()
        .any(|event| event.fact_id() != query.fact_id() || event.owner() != query.owner())
    {
        return Err(invalid("stored lineage event identity mismatch"));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ComponentVersion, EvidenceClass, FactCategoryV1, FactEvidenceRefV1, FactEvidenceRelationV1,
        FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, PayloadReferenceV1,
        ProvenanceId, RetentionClass, RetrievalAnchorId, SanitizationReceiptId,
        SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    };

    /// Every table `insert_assertion` writes or compares against, so the write
    /// path is exercised with the real column set rather than a stub.
    fn assertion_schema(connection: &rusqlite::Connection) {
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_access TEXT NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertions (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    assertion_header_json TEXT NOT NULL,
                    kind_json TEXT NOT NULL,
                    payload_reference_json TEXT NOT NULL,
                    receipt_json TEXT NOT NULL,
                    asserted_at INTEGER NOT NULL,
                    actor_id TEXT,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertion_supersession (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    superseded_assertion_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id, ordinal)
                 );
                 CREATE TABLE memory_v2_assertion_payloads (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    content TEXT NOT NULL,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_evidence (
                    evidence_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    anchor_id TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    PRIMARY KEY (evidence_id, fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertion_evidence (
                    assertion_id TEXT NOT NULL,
                    evidence_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id, ordinal)
                 );",
            )
            .unwrap();
    }

    fn payload(content: &str) -> FactPayloadV1 {
        let material = serde_json::json!({
            "content": content,
            "category": "project",
            "tags": ["fact-executor"],
            "entities": ["TraceDecay"],
            "metadata": {},
        });
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.fact-executor").unwrap(),
                ComponentVersion::new("sanitizer.fact-executor.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&material).unwrap()),
        )
        .unwrap();
        FactPayloadV1::new(
            content.to_owned(),
            FactCategoryV1::Project,
            vec!["fact-executor".to_owned()],
            vec!["TraceDecay".to_owned()],
            serde_json::json!({}),
            receipt,
            RetentionClass::new("durable.fact-executor").unwrap(),
        )
        .unwrap()
    }

    fn evidence_ref(fact_id: &FactId, anchor: &str) -> FactEvidenceRefV1 {
        FactEvidenceRefV1::new(
            fact_id.clone(),
            RetrievalAnchorId::new(anchor).unwrap(),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap()
    }

    fn assertion(
        fact_id: &FactId,
        content: &str,
        evidence: Vec<FactEvidenceRefV1>,
    ) -> FactAssertionV1 {
        FactAssertionV1::new(
            fact_id.clone(),
            FactOwnerV1::Profile,
            FactAssertionKindV1::Initial,
            payload(content),
            evidence,
            UtcMicros(5),
            None,
        )
        .unwrap()
    }

    #[test]
    fn assertion_replay_is_idempotent_and_reuse_is_a_collision() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        assertion_schema(&connection);
        let owner = OwnerColumns::new(&FactOwnerV1::Profile).unwrap();
        let fact_id = profile_fact_id("operation.assertion-replay");
        let anchor = "retrieval.fact-executor.alpha";
        let assertion = assertion(
            &fact_id,
            "assertion replay",
            vec![evidence_ref(&fact_id, anchor)],
        );
        let savepoint = connection.savepoint().unwrap();

        insert_assertion(&savepoint, &owner, &assertion).unwrap();
        // Exact replay of a stored assertion is a no-op, not a primary-key
        // violation surfaced from the driver.
        insert_assertion(&savepoint, &owner, &assertion).unwrap();
        let assertions = savepoint
            .query_row(
                "SELECT COUNT(*) FROM memory_v2_assertions WHERE assertion_id = ?1",
                [assertion.assertion_id().as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(assertions, 1, "replay must not append a second assertion");

        // The same assertion id bound to different stored content is a
        // collision, classified exactly as the root commit engine classifies it.
        savepoint
            .execute(
                "UPDATE memory_v2_assertions SET asserted_at = asserted_at + 1
                 WHERE assertion_id = ?1",
                [assertion.assertion_id().as_str()],
            )
            .unwrap();
        let error = insert_assertion(&savepoint, &owner, &assertion).unwrap_err();
        assert!(
            error.to_string().contains("assertion identity collision"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn evidence_rebound_to_another_anchor_is_a_collision() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        assertion_schema(&connection);
        let owner = OwnerColumns::new(&FactOwnerV1::Profile).unwrap();
        let fact_id = profile_fact_id("operation.evidence-rebound");
        let evidence = evidence_ref(&fact_id, "retrieval.fact-executor.alpha");
        let assertion = assertion(&fact_id, "evidence rebound", vec![evidence.clone()]);
        let savepoint = connection.savepoint().unwrap();
        // A stored evidence row that reuses the evidence id against a different
        // anchor must not be silently adopted by `INSERT OR IGNORE`.
        savepoint
            .execute(
                "INSERT INTO memory_v2_evidence (
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    fact_id.as_str(),
                    owner.kind,
                    owner.project_id,
                    owner.json,
                    "retrieval.fact-executor.beta",
                    encode(&evidence).unwrap(),
                ],
            )
            .unwrap();

        let error = insert_assertion(&savepoint, &owner, &assertion).unwrap_err();
        assert!(
            error.to_string().contains("evidence identity collision"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn evidence_exact_replay_is_accepted() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        assertion_schema(&connection);
        let owner = OwnerColumns::new(&FactOwnerV1::Profile).unwrap();
        let fact_id = profile_fact_id("operation.evidence-replay");
        let anchor = "retrieval.fact-executor.alpha";
        let evidence = evidence_ref(&fact_id, anchor);
        let assertion = assertion(&fact_id, "evidence replay", vec![evidence.clone()]);
        let savepoint = connection.savepoint().unwrap();
        savepoint
            .execute(
                "INSERT INTO memory_v2_evidence (
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    fact_id.as_str(),
                    owner.kind,
                    owner.project_id,
                    owner.json,
                    anchor,
                    encode(&evidence).unwrap(),
                ],
            )
            .unwrap();

        insert_assertion(&savepoint, &owner, &assertion).unwrap();
        let linked = savepoint
            .query_row(
                "SELECT COUNT(*) FROM memory_v2_assertion_evidence
                 WHERE assertion_id = ?1 AND evidence_id = ?2",
                params![
                    assertion.assertion_id().as_str(),
                    evidence.evidence_id().as_str(),
                ],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(linked, 1, "identical evidence must still link the assertion");
    }

    fn profile_fact_id(operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new(operation).unwrap(),
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn fact_write_rejects_stored_identity_mismatch() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    last_event_id TEXT NOT NULL
                 );
                 CREATE TABLE memory_v2_facts (
                    fact_id TEXT PRIMARY KEY,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    identity_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
        let owner = FactOwnerV1::Profile;
        let requested_identity = FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Application {
                operation_id: ProvenanceId::new("operation.requested").unwrap(),
            },
        )
        .unwrap();
        let requested_fact_id = FactId::derive(&requested_identity).unwrap();
        let stored_identity = FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Application {
                operation_id: ProvenanceId::new("operation.other").unwrap(),
            },
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO memory_v2_facts (
                    fact_id, owner_kind, project_id, owner_json, identity_json, created_at
                 ) VALUES (?1, 'profile', '', ?2, ?3, 1)",
                params![
                    requested_fact_id.as_str(),
                    serde_json::to_string(&owner).unwrap(),
                    serde_json::to_string(&stored_identity).unwrap(),
                ],
            )
            .unwrap();
        let event = FactLineageEventV1::new(
            requested_fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(2),
            None,
        )
        .unwrap();
        let batch = FactWriteBatch::new(
            requested_fact_id,
            owner,
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap()
        .with_identity_material(requested_identity)
        .unwrap();
        let savepoint = connection.savepoint().unwrap();

        let error = FactExecutor.execute_write(&savepoint, &batch).unwrap_err();
        assert!(error.to_string().contains("fact identity collision"));
    }

    #[test]
    fn fact_executor_does_not_claim_replay_without_writer_ledger() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    last_event_id TEXT NOT NULL
                 );
                 CREATE TABLE memory_v2_lineage_events (
                    event_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    event_json TEXT NOT NULL,
                    occurred_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
        let owner = FactOwnerV1::Profile;
        let fact_id = profile_fact_id("operation.writer-ledger");
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(2),
            None,
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO memory_v2_current_facts
                    (fact_id, owner_kind, project_id, last_event_id)
                 VALUES (?1, 'profile', '', ?2)",
                params![fact_id.as_str(), event.event_id().as_str()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO memory_v2_lineage_events
                    (event_id, fact_id, owner_kind, project_id, event_json, occurred_at)
                 VALUES (?1, ?2, 'profile', '', ?3, ?4)",
                params![
                    event.event_id().as_str(),
                    fact_id.as_str(),
                    serde_json::to_string(&event).unwrap(),
                    event.occurred_at().0,
                ],
            )
            .unwrap();
        let batch = FactWriteBatch::new(
            fact_id,
            owner,
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap();
        let savepoint = connection.savepoint().unwrap();

        let error = FactExecutor.execute_write(&savepoint, &batch).unwrap_err();
        assert!(error.to_string().contains("last-event conflict"));
    }

    #[test]
    fn purge_access_transition_clears_active_assertion() {
        for current in [PayloadAccessState::Quarantined, PayloadAccessState::Deleted] {
            let mut connection = rusqlite::Connection::open_in_memory().unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE memory_v2_current_facts (
                        fact_id TEXT NOT NULL,
                        owner_kind TEXT NOT NULL,
                        project_id TEXT NOT NULL,
                        payload_access TEXT NOT NULL,
                        trust_score REAL,
                        active_assertion_id TEXT,
                        last_event_id TEXT NOT NULL,
                        updated_at INTEGER NOT NULL,
                        PRIMARY KEY (fact_id, owner_kind, project_id)
                    );",
                )
                .unwrap();
            let owner = FactOwnerV1::Profile;
            let owner_columns = OwnerColumns::new(&owner).unwrap();
            let fact_id = profile_fact_id("operation.purge-projection");
            connection
                .execute(
                    "INSERT INTO memory_v2_current_facts (
                        fact_id, owner_kind, project_id, payload_access, trust_score,
                        active_assertion_id, last_event_id, updated_at
                     ) VALUES (?1, 'profile', '', 'eligible', 0.8, ?2, ?3, 1)",
                    params![
                        fact_id.as_str(),
                        FactAssertionId::new("assertion.active").unwrap().as_str(),
                        FactEventId::new("event.previous").unwrap().as_str(),
                    ],
                )
                .unwrap();
            let event = FactLineageEventV1::new(
                fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::PayloadAccessChanged {
                    previous: PayloadAccessState::Eligible,
                    current,
                },
                UtcMicros(2),
                None,
            )
            .unwrap();
            let batch = FactWriteBatch::new(
                fact_id.clone(),
                owner,
                None,
                vec![event],
                vec![],
                vec![],
                None,
                None,
            )
            .unwrap();
            let savepoint = connection.savepoint().unwrap();

            publish_projection(&savepoint, &owner_columns, &batch).unwrap();
            let active = savepoint
                .query_row(
                    "SELECT active_assertion_id FROM memory_v2_current_facts
                     WHERE fact_id = ?1 AND owner_kind = 'profile' AND project_id = ''",
                    [fact_id.as_str()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap();

            assert_eq!(active, None, "{current:?} must purge the active assertion");
        }
    }

    #[test]
    fn stale_projection_transitions_are_rejected() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_access TEXT NOT NULL,
                    trust_score REAL,
                    active_assertion_id TEXT,
                    last_event_id TEXT NOT NULL,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                );",
            )
            .unwrap();
        let owner = FactOwnerV1::Profile;
        let owner_columns = OwnerColumns::new(&owner).unwrap();
        let fact_id = profile_fact_id("operation.stale-projection");
        connection
            .execute(
                "INSERT INTO memory_v2_current_facts (
                    fact_id, owner_kind, project_id, payload_access, trust_score,
                    active_assertion_id, last_event_id, updated_at
                 ) VALUES (?1, 'profile', '', 'eligible', 0.8, ?2, ?3, 1)",
                params![
                    fact_id.as_str(),
                    FactAssertionId::new("assertion.active").unwrap().as_str(),
                    FactEventId::new("event.previous").unwrap().as_str(),
                ],
            )
            .unwrap();
        let stale_trust = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: Confidence::new(0.7).unwrap(),
                current: Confidence::new(0.9).unwrap(),
                evidence_ids: vec![],
            },
            UtcMicros(2),
            None,
        )
        .unwrap();
        let stale_access = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Redacted,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(3),
            None,
        )
        .unwrap();

        for event in [stale_trust, stale_access] {
            let batch = FactWriteBatch::new(
                fact_id.clone(),
                owner.clone(),
                None,
                vec![event],
                vec![],
                vec![],
                None,
                None,
            )
            .unwrap();
            let savepoint = connection.savepoint().unwrap();

            assert!(publish_projection(&savepoint, &owner_columns, &batch).is_err());
        }
    }

    #[test]
    fn current_read_omits_fact_without_active_assertion() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_access TEXT NOT NULL,
                    trust_score REAL,
                    active_assertion_id TEXT,
                    last_event_id TEXT NOT NULL,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertion_payloads (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL
                 );
                 CREATE TABLE memory_v2_legacy_map (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    mapping_json TEXT NOT NULL
                 );",
            )
            .unwrap();
        let owner = FactOwnerV1::Profile;
        let fact_id = profile_fact_id("operation.current-after-purge");
        connection
            .execute(
                "INSERT INTO memory_v2_facts
                    (fact_id, owner_kind, project_id, owner_json)
                 VALUES (?1, 'profile', '', ?2)",
                params![fact_id.as_str(), encode(&owner).unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO memory_v2_current_facts (
                    fact_id, owner_kind, project_id, payload_access, trust_score,
                    active_assertion_id, last_event_id, updated_at
                 ) VALUES (?1, 'profile', '', 'deleted', 0.8, NULL, ?2, 2)",
                params![
                    fact_id.as_str(),
                    FactEventId::new("event.deleted").unwrap().as_str(),
                ],
            )
            .unwrap();
        let query = FactCurrentQuery::new(owner, fact_id).unwrap();

        assert_eq!(read_current(&connection, &query).unwrap(), None);
    }

    #[test]
    fn lineage_read_rejects_stored_event_identity_mismatch() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_lineage_events (
                    event_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    event_json TEXT NOT NULL,
                    occurred_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        let requested_fact_id = profile_fact_id("operation.requested");
        let stored_event = FactLineageEventV1::new(
            profile_fact_id("operation.other"),
            FactOwnerV1::Profile,
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(7),
            None,
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO memory_v2_lineage_events (
                    event_id, fact_id, owner_kind, project_id, event_json, occurred_at
                 ) VALUES (?1, ?2, 'profile', '', ?3, ?4)",
                params![
                    stored_event.event_id().as_str(),
                    requested_fact_id.as_str(),
                    serde_json::to_string(&stored_event).unwrap(),
                    stored_event.occurred_at().0,
                ],
            )
            .unwrap();
        let query =
            FactLineageQuery::new(FactOwnerV1::Profile, requested_fact_id, None, 10).unwrap();

        assert!(read_lineage(&connection, &query).is_err());
    }
}
