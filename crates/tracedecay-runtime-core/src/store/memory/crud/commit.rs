//! Canonical fact commit engine: append order, identity, anchors, and assertions.

use super::super::primitives::{
    COMMIT_OPERATION, OwnerKey, QUERY_OPERATION, row_exists, row_i64, row_optional_string,
    row_string, storage_error, storage_message, to_json,
};
use super::super::privacy_purge::{
    assertion_payload_is_explicitly_purged_tx, purge_superseded_payloads_for_fact_tx,
};
use super::{
    CommitAttempt, ensure_event_references, ensure_fact_identity, event_exists, event_matches,
    insert_event, payload_is_purged_projection, publish_current_projection, receipt_outcome,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde::Serialize;
use tracedecay_domain::{
    FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactEventId, FactId, FactLineageEventV1,
    FactOwnerV1, RetrievalAnchorId, RetrievalAnchorRecordV2, UtcMicros,
};
use tracedecay_store::{
    FactCommitConflict, FactCommitOutcome, FactStoreError, FactStoreResult, FactWriteBatch,
};
/// The immutable assertion record deliberately excludes `FactPayloadV1`.
/// Payload bytes belong only in `memory_v2_assertion_payloads`, which is the
/// storage locus erased when an access transition reaches `Deleted`.
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

fn assertion_header_json(assertion: &FactAssertionV1) -> FactStoreResult<String> {
    let payload_reference = assertion.payload().payload_reference()?;
    to_json(
        &StoredAssertionHeaderV1 {
            assertion_id: assertion.assertion_id(),
            fact_id: assertion.fact_id(),
            owner: assertion.owner(),
            kind: assertion.kind(),
            payload_reference: &payload_reference,
            evidence: assertion.evidence(),
            asserted_at: assertion.asserted_at(),
            actor_id: assertion.actor_id(),
        },
        "serialize payload-free fact assertion header",
    )
}

pub(super) async fn commit_fact_tx(
    transaction: &Transaction<'_>,
    batch: &FactWriteBatch,
) -> FactStoreResult<CommitAttempt> {
    let owner = OwnerKey::new(batch.owner())?;
    let actual_last = current_last_event(transaction, &owner, batch.fact_id()).await?;
    if batch_is_exact_replay(transaction, &owner, batch, actual_last.as_ref()).await? {
        return Ok(CommitAttempt {
            outcome: receipt_outcome(transaction, &owner, batch, true).await?,
            wrote: false,
        });
    }
    if let Some(conflict) = batch_identity_collision(transaction, &owner, batch).await? {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(conflict),
            wrote: false,
        });
    }
    if actual_last.as_ref() != batch.expected_last_event_id() {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(FactCommitConflict::LastEventMismatch {
                expected: batch.expected_last_event_id().cloned(),
                actual: actual_last,
            }),
            wrote: false,
        });
    }
    ensure_append_order(transaction, &owner, batch, actual_last.as_ref()).await?;

    ensure_fact_identity(transaction, &owner, batch).await?;
    ensure_referenced_anchors(transaction, &owner, batch).await?;
    for anchor in batch.new_anchors() {
        insert_or_verify_anchor(transaction, &owner, anchor).await?;
    }
    if let Some(assertion) = batch.assertion() {
        insert_assertion(transaction, &owner, assertion).await?;
    }
    for event in batch.events() {
        ensure_event_references(transaction, &owner, event).await?;
    }
    for event in batch.events() {
        insert_event(transaction, &owner, event).await?;
    }
    publish_current_projection(transaction, &owner, batch).await?;
    if let Some(assertion) = batch.assertion() {
        purge_superseded_payloads_for_fact_tx(
            transaction,
            &owner,
            batch.fact_id(),
            assertion.assertion_id(),
        )
        .await?;
    }

    Ok(CommitAttempt {
        outcome: receipt_outcome(transaction, &owner, batch, false).await?,
        wrote: true,
    })
}

pub(super) async fn current_last_event(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<Option<FactEventId>> {
    let mut rows = transaction
        .query(
            "SELECT last_event_id FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    Ok(Some(FactEventId::new(row_string(
        &row,
        0,
        QUERY_OPERATION,
    )?)?))
}

async fn ensure_append_order(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<()> {
    let Some(last_event_id) = actual_last else {
        return Ok(());
    };
    let first = batch.events().first().ok_or(FactStoreError::EmptyBatch)?;
    let mut rows = transaction
        .query(
            "SELECT occurred_at, event_id FROM memory_v2_lineage_events
             WHERE event_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                last_event_id.as_str(),
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        .ok_or_else(|| storage_message(COMMIT_OPERATION, "current fact points at missing event"))?;
    let last = (
        UtcMicros(row_i64(&row, 0, COMMIT_OPERATION)?),
        FactEventId::new(row_string(&row, 1, COMMIT_OPERATION)?)?,
    );
    if (first.occurred_at(), first.event_id()) <= (last.0, &last.1) {
        return Err(FactStoreError::EventsOutOfOrder);
    }
    Ok(())
}

async fn batch_is_exact_replay(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<bool> {
    if actual_last != batch.events().last().map(FactLineageEventV1::event_id) {
        return Ok(false);
    }
    if !fact_identity_matches(transaction, owner, batch).await? {
        return Ok(false);
    }
    for anchor in batch.new_anchors() {
        if !anchor_matches(transaction, owner, anchor).await? {
            return Ok(false);
        }
    }
    if let Some(assertion) = batch.assertion()
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(false);
    }
    for event in batch.events() {
        if !event_matches(transaction, owner, event).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn batch_identity_collision(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<Option<FactCommitConflict>> {
    if fact_exists(transaction, batch.fact_id()).await?
        && !fact_identity_matches(transaction, owner, batch).await?
    {
        return Ok(Some(collision("fact", batch.fact_id().as_str())));
    }
    for anchor in batch.new_anchors() {
        if anchor_exists(transaction, anchor.anchor_id()).await?
            && !anchor_matches(transaction, owner, anchor).await?
        {
            return Ok(Some(collision(
                "retrieval anchor",
                anchor.anchor_id().as_str(),
            )));
        }
    }
    if let Some(assertion) = batch.assertion()
        && assertion_exists(transaction, assertion.assertion_id()).await?
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(Some(collision(
            "assertion",
            assertion.assertion_id().as_str(),
        )));
    }
    for event in batch.events() {
        if event_exists(transaction, event.event_id()).await?
            && !event_matches(transaction, owner, event).await?
        {
            return Ok(Some(collision("event", event.event_id().as_str())));
        }
    }
    Ok(None)
}

pub(super) fn collision(kind: &'static str, id: &str) -> FactCommitConflict {
    FactCommitConflict::IdentityCollision {
        kind,
        id: id.to_owned(),
    }
}

async fn fact_exists(transaction: &Transaction<'_>, fact_id: &FactId) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_facts WHERE fact_id = ?1",
        [fact_id.as_str()],
    )
    .await
}

async fn fact_identity_matches(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT owner_kind, project_id, owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    let identity_matches = match batch.identity_material() {
        Some(identity) => {
            row_string(&row, 3, QUERY_OPERATION)? == to_json(identity, "serialize fact identity")?
        }
        None => true,
    };
    Ok(row_string(&row, 0, QUERY_OPERATION)? == owner.kind
        && row_string(&row, 1, QUERY_OPERATION)? == owner.project_id
        && row_string(&row, 2, QUERY_OPERATION)? == owner.json
        && identity_matches)
}

async fn ensure_referenced_anchors(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    for anchor_id in batch.referenced_anchor_ids() {
        let mut rows = transaction
            .query(
                "SELECT 1 FROM retrieval_anchors AS anchor
                 WHERE anchor.anchor_id = ?1 AND anchor.owner_json = ?2
                   AND COALESCE((
                       SELECT disposition.state
                       FROM retrieval_anchor_dispositions AS disposition
                       WHERE disposition.anchor_id = anchor.anchor_id
                         AND disposition.owner_json = anchor.owner_json
                       ORDER BY disposition.sequence DESC LIMIT 1
                   ), 'active') = 'active'",
                params![anchor_id.as_str(), owner.json.as_str()],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        let Some(_row) = rows
            .next()
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        else {
            return Err(FactStoreError::MissingEvidenceAnchor {
                anchor_id: anchor_id.clone(),
            });
        };
    }
    Ok(())
}

async fn insert_or_verify_anchor(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<()> {
    if anchor_exists(transaction, anchor.anchor_id()).await? {
        if anchor_matches(transaction, owner, anchor).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "retrieval anchor identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO retrieval_anchors(
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES(?1, ?2, ?3, ?4)",
            params![
                anchor.anchor_id().as_str(),
                to_json(anchor, "serialize retrieval anchor")?,
                owner.json.as_str(),
                anchor.projection_generation().as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    for alias in anchor.aliases() {
        transaction
            .execute(
                "INSERT INTO retrieval_anchor_aliases(
                    owner_json, alias_kind, locator_digest, anchor_id
                 ) VALUES(?1, ?2, ?3, ?4)",
                params![
                    owner.json.as_str(),
                    to_json(&alias.kind(), "serialize anchor alias kind")?,
                    to_json(alias.locator_digest(), "serialize anchor locator digest")?,
                    anchor.anchor_id().as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

async fn anchor_exists(
    transaction: &Transaction<'_>,
    anchor_id: &RetrievalAnchorId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM retrieval_anchors WHERE anchor_id = ?1",
        [anchor_id.as_str()],
    )
    .await
}

pub(super) async fn anchor_matches(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            [anchor.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    if row_string(&row, 0, QUERY_OPERATION)? != to_json(anchor, "serialize retrieval anchor")?
        || row_string(&row, 1, QUERY_OPERATION)? != owner.json
        || row_string(&row, 2, QUERY_OPERATION)? != anchor.projection_generation().as_str()
    {
        return Ok(false);
    }
    let mut aliases = transaction
        .query(
            "SELECT alias_kind, locator_digest FROM retrieval_anchor_aliases
             WHERE anchor_id = ?1 ORDER BY alias_kind, locator_digest",
            [anchor.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored = Vec::new();
    while let Some(row) = aliases
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
        ));
    }
    let mut expected = anchor
        .aliases()
        .iter()
        .map(|alias| {
            Ok((
                to_json(&alias.kind(), "serialize anchor alias kind")?,
                to_json(alias.locator_digest(), "serialize anchor locator digest")?,
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    expected.sort();
    Ok(stored == expected)
}

async fn insert_assertion(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<()> {
    if assertion_exists(transaction, assertion.assertion_id()).await? {
        if assertion_matches(transaction, owner, assertion).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "assertion identity collision",
        ));
    }
    let header_json = assertion_header_json(assertion)?;
    let actor_id = assertion.actor_id().map(ToString::to_string);
    transaction
        .execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                header_json,
                to_json(assertion.kind(), "serialize assertion kind")?,
                to_json(
                    &assertion.payload().payload_reference()?,
                    "serialize assertion payload reference",
                )?,
                to_json(assertion.payload().receipt(), "serialize assertion receipt")?,
                assertion.asserted_at().0,
                actor_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, superseded) in superseded_assertions(assertion.kind()).iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_supersession(
                    assertion_id, fact_id, owner_kind, project_id,
                    superseded_assertion_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    superseded.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }

    transaction
        .execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                to_json(assertion.payload(), "serialize assertion payload")?,
                assertion.payload().content(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, evidence) in assertion.evidence().iter().enumerate() {
        let evidence_json = to_json(evidence, "serialize fact evidence")?;
        let changed = transaction
            .execute(
                "INSERT OR IGNORE INTO memory_v2_evidence(
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    owner.json.as_str(),
                    evidence.anchor_id().as_str(),
                    evidence_json.as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        if changed == 0 {
            let mut rows = transaction
                .query(
                    "SELECT evidence_json, owner_json, anchor_id
                     FROM memory_v2_evidence
                     WHERE evidence_id = ?1 AND fact_id = ?2
                       AND owner_kind = ?3 AND project_id = ?4",
                    params![
                        evidence.evidence_id().as_str(),
                        assertion.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?
            else {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence insert disappeared",
                ));
            };
            if row_string(&row, 0, COMMIT_OPERATION)? != evidence_json
                || row_string(&row, 1, COMMIT_OPERATION)? != owner.json
                || row_string(&row, 2, COMMIT_OPERATION)? != evidence.anchor_id().as_str()
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence identity collision",
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_evidence(
                    assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

fn superseded_assertions(kind: &FactAssertionKindV1) -> Vec<&FactAssertionId> {
    match kind {
        FactAssertionKindV1::Correction { supersedes } => vec![supersedes],
        FactAssertionKindV1::Merge { supersedes } => supersedes.iter().collect(),
        FactAssertionKindV1::Initial => Vec::new(),
    }
}

async fn assertion_exists(
    transaction: &Transaction<'_>,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_assertions WHERE assertion_id = ?1",
        [assertion_id.as_str()],
    )
    .await
}

async fn assertion_matches(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_kind, project_id, owner_json,
                    assertion_header_json, kind_json, payload_reference_json,
                    receipt_json, asserted_at, actor_id
             FROM memory_v2_assertions WHERE assertion_id = ?1",
            [assertion.assertion_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    let stored_actor = row_optional_string(&row, 9, QUERY_OPERATION)?;
    let expected_actor = assertion.actor_id().map(ToString::to_string);
    if row_string(&row, 0, QUERY_OPERATION)? != assertion.fact_id().as_str()
        || row_string(&row, 1, QUERY_OPERATION)? != owner.kind
        || row_string(&row, 2, QUERY_OPERATION)? != owner.project_id
        || row_string(&row, 3, QUERY_OPERATION)? != owner.json
        || row_string(&row, 4, QUERY_OPERATION)? != assertion_header_json(assertion)?
        || row_string(&row, 5, QUERY_OPERATION)?
            != to_json(assertion.kind(), "serialize assertion kind")?
        || row_string(&row, 6, QUERY_OPERATION)?
            != to_json(
                &assertion.payload().payload_reference()?,
                "serialize assertion payload reference",
            )?
        || row_string(&row, 7, QUERY_OPERATION)?
            != to_json(assertion.payload().receipt(), "serialize assertion receipt")?
        || row_i64(&row, 8, QUERY_OPERATION)? != assertion.asserted_at().0
        || stored_actor != expected_actor
    {
        return Ok(false);
    }

    let mut supersession = transaction
        .query(
            "SELECT superseded_assertion_id FROM memory_v2_assertion_supersession
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 ORDER BY ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_supersession = Vec::new();
    while let Some(row) = supersession
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_supersession.push(row_string(&row, 0, QUERY_OPERATION)?);
    }
    let expected_supersession = superseded_assertions(assertion.kind())
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect::<Vec<_>>();
    if stored_supersession != expected_supersession {
        return Ok(false);
    }

    let mut payload = transaction
        .query(
            "SELECT payload_json, content FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let payload_row = payload
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    drop(payload);
    let payload_matches = match payload_row {
        Some(row) => {
            row_string(&row, 0, QUERY_OPERATION)?
                == to_json(assertion.payload(), "serialize assertion payload")?
                && row_string(&row, 1, QUERY_OPERATION)? == assertion.payload().content()
        }
        // A missing payload row is consistent only with a terminal fact-wide
        // purge or an immutable assertion-specific detector purge receipt.
        None => {
            payload_is_purged_projection(transaction, owner, assertion.fact_id()).await?
                || assertion_payload_is_explicitly_purged_tx(transaction, owner, assertion).await?
        }
    };
    if !payload_matches {
        return Ok(false);
    }

    let mut evidence = transaction
        .query(
            "SELECT ae.evidence_id, e.evidence_json, e.owner_json, e.anchor_id
             FROM memory_v2_assertion_evidence ae
             JOIN memory_v2_evidence e ON
                e.evidence_id = ae.evidence_id AND e.fact_id = ae.fact_id AND
                e.owner_kind = ae.owner_kind AND e.project_id = ae.project_id
             WHERE ae.assertion_id = ?1 AND ae.fact_id = ?2
               AND ae.owner_kind = ?3 AND ae.project_id = ?4 ORDER BY ae.ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_evidence = Vec::new();
    while let Some(row) = evidence
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_evidence.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
            row_string(&row, 2, QUERY_OPERATION)?,
            row_string(&row, 3, QUERY_OPERATION)?,
        ));
    }
    let expected_evidence = assertion
        .evidence()
        .iter()
        .map(|evidence| {
            Ok((
                evidence.evidence_id().as_str().to_owned(),
                to_json(evidence, "serialize fact evidence")?,
                owner.json.clone(),
                evidence.anchor_id().as_str().to_owned(),
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    Ok(stored_evidence == expected_evidence)
}
