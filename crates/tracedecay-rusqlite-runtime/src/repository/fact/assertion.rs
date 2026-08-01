//! Persisting one fact assertion, and replaying one against what is stored.
//!
//! [`PersistedAssertion`] is derived once and drives both directions, so the
//! row an insert writes and the row a replay is compared against cannot drift
//! apart.

use rusqlite::{OptionalExtension, Savepoint, params, params_from_iter};
use serde::Serialize;
use tracedecay_domain::{
    FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactId, FactOwnerV1, PayloadAccessState,
    UtcMicros,
};

use super::super::support::{
    Column, ColumnValue, decode, encode, idempotent_insert, insert_row, invalid,
    stored_row_matches, usize_to_i64,
};
use super::OwnerColumns;

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

/// Everything the write path persists for one assertion, derived once.
///
/// Both the insert and the replay comparison read from this, so the stored row
/// and the row a replay is checked against cannot drift apart.
struct PersistedAssertion<'a> {
    /// The four columns every row of an assertion is filed under.
    scope: Vec<Column<'a>>,
    /// `memory_v2_assertions` keyed by `assertion_id` alone, matching the
    /// `UNIQUE(assertion_id, owner_json)` that a colliding write would trip.
    header: Vec<Column<'a>>,
    supersession: Vec<String>,
    payload: Vec<Column<'a>>,
    evidence: Vec<EvidenceRow>,
}

/// One assertion evidence row in canonical ordinal order.
type EvidenceRow = (String, String, String, String);

impl<'a> PersistedAssertion<'a> {
    fn derive(owner: &OwnerColumns, assertion: &FactAssertionV1) -> rusqlite::Result<Self> {
        let payload_reference = assertion.payload().payload_reference().map_err(invalid)?;
        Ok(Self {
            scope: vec![
                ("assertion_id", assertion.assertion_id().as_str().into()),
                ("fact_id", assertion.fact_id().as_str().into()),
                ("owner_kind", owner.kind.into()),
                ("project_id", owner.project_id.clone().into()),
            ],
            header: vec![
                ("fact_id", assertion.fact_id().as_str().into()),
                ("owner_kind", owner.kind.into()),
                ("project_id", owner.project_id.clone().into()),
                ("owner_json", owner.json.clone().into()),
                (
                    "assertion_header_json",
                    assertion_header_json(assertion)?.into(),
                ),
                ("kind_json", encode(assertion.kind())?.into()),
                ("payload_reference_json", encode(&payload_reference)?.into()),
                (
                    "receipt_json",
                    encode(assertion.payload().receipt())?.into(),
                ),
                ("asserted_at", assertion.asserted_at().0.into()),
                (
                    "actor_id",
                    assertion.actor_id().map(|actor| actor.as_str()).into(),
                ),
            ],
            supersession: superseded_assertions(assertion.kind())
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            payload: vec![
                ("payload_json", encode(assertion.payload())?.into()),
                ("content", assertion.payload().content().into()),
            ],
            evidence: assertion
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
                .collect::<rusqlite::Result<Vec<_>>>()?,
        })
    }

    fn header_key(&self) -> &[Column<'a>] {
        &self.scope[..1]
    }

    fn scope_bindings(&self) -> impl Iterator<Item = &ColumnValue> {
        self.scope.iter().map(|(_, value)| value)
    }
}

pub(super) fn insert_assertion(
    savepoint: &Savepoint<'_>,
    owner: &OwnerColumns,
    assertion: &FactAssertionV1,
) -> rusqlite::Result<()> {
    let persisted = PersistedAssertion::derive(owner, assertion)?;
    if let Some(header_matches) = stored_row_matches(
        savepoint,
        "memory_v2_assertions",
        persisted.header_key(),
        &persisted.header,
    )? {
        return if header_matches
            && assertion_children_match(savepoint, owner, assertion.fact_id(), &persisted)?
        {
            Ok(())
        } else {
            Err(invalid("assertion identity collision"))
        };
    }
    insert_row(
        savepoint,
        "memory_v2_assertions",
        &[persisted.header_key(), &persisted.header].concat(),
    )?;
    for (ordinal, superseded) in persisted.supersession.iter().enumerate() {
        insert_row(
            savepoint,
            "memory_v2_assertion_supersession",
            &[
                persisted.scope.as_slice(),
                &[
                    ("superseded_assertion_id", superseded.as_str().into()),
                    (
                        "ordinal",
                        usize_to_i64(ordinal, "assertion supersession ordinal")?.into(),
                    ),
                ],
            ]
            .concat(),
        )?;
    }
    insert_row(
        savepoint,
        "memory_v2_assertion_payloads",
        &[persisted.scope.as_slice(), &persisted.payload].concat(),
    )?;
    for (ordinal, (evidence_id, evidence_json, owner_json, anchor_id)) in
        persisted.evidence.iter().enumerate()
    {
        idempotent_insert(
            savepoint,
            "memory_v2_evidence",
            &[
                ("evidence_id", evidence_id.as_str().into()),
                ("fact_id", assertion.fact_id().as_str().into()),
                ("owner_kind", owner.kind.into()),
                ("project_id", owner.project_id.clone().into()),
            ],
            &[
                ("owner_json", owner_json.as_str().into()),
                ("anchor_id", anchor_id.as_str().into()),
                ("evidence_json", evidence_json.as_str().into()),
            ],
            "evidence identity collision",
        )?;
        insert_row(
            savepoint,
            "memory_v2_assertion_evidence",
            &[
                persisted.scope.as_slice(),
                &[
                    ("evidence_id", evidence_id.as_str().into()),
                    (
                        "ordinal",
                        usize_to_i64(ordinal, "assertion evidence ordinal")?.into(),
                    ),
                ],
            ]
            .concat(),
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

/// Compare the rows hanging off a stored assertion header against the ones the
/// caller would have written: the supersession list, the payload, and the
/// ordered evidence.
///
/// This mirrors the root commit engine so an exact replay is idempotent and a
/// reused assertion id with different content is a collision, rather than a raw
/// primary-key violation from the driver.
fn assertion_children_match(
    connection: &rusqlite::Connection,
    owner: &OwnerColumns,
    fact_id: &FactId,
    persisted: &PersistedAssertion<'_>,
) -> rusqlite::Result<bool> {
    let mut supersession = connection.prepare(
        "SELECT superseded_assertion_id FROM memory_v2_assertion_supersession
         WHERE assertion_id = ?1 AND fact_id = ?2
           AND owner_kind = ?3 AND project_id = ?4 ORDER BY ordinal",
    )?;
    let stored_supersession = supersession
        .query_map(params_from_iter(persisted.scope_bindings()), |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if stored_supersession != persisted.supersession {
        return Ok(false);
    }

    let payload_matches = stored_row_matches(
        connection,
        "memory_v2_assertion_payloads",
        &persisted.scope,
        &persisted.payload,
    )?;
    match payload_matches {
        Some(false) => return Ok(false),
        // A missing payload row is only consistent with a purged projection.
        None if !payload_is_purged_projection(connection, owner, fact_id)? => return Ok(false),
        Some(true) | None => {}
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
    let stored_evidence = evidence
        .query_map(params_from_iter(persisted.scope_bindings()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(stored_evidence == persisted.evidence)
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
