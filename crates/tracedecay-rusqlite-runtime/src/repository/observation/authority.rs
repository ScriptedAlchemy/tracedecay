//! The authority rows an observation write persists and replays against.
//!
//! Each `persist_*` here is paired with the `verify_*` that a replay runs
//! instead, so a re-applied write reads back exactly the rows the first apply
//! wrote or fails as a collision.

use rusqlite::{OptionalExtension, params};
use tracedecay_domain::{
    AnchorSourceGenerationV2, DurableObservationV1, EvidenceAvailabilityV1, FactOwnerV1,
    GenerationBoundRepositoryProvenanceV1, ObservationSourceCursorV1, RepositoryProvenanceV1,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    prove_cline_native_source_transition,
};
use tracedecay_store::{
    AnchorDispositionReasonClassV1, AnchorDispositionStateV1, AnchoredObservationWrite,
    ObservationCursorAdvance, RepositoryProvenanceAttachmentV1, RetrievalAnchorDispositionRecordV1,
};

use super::super::support::{decode, encode, invalid};
use super::cursor_authority::{
    READ_CURSOR_ADVANCE_SQL, READ_SOURCE_CURSOR_SQL, cursor_advance_ledger_row_matches,
};

pub(super) fn persist_sanitization_receipt(
    connection: &rusqlite::Connection,
    receipt: &tracedecay_domain::SanitizationReceiptV1,
) -> rusqlite::Result<()> {
    let receipt_json = encode(receipt)?;
    let receipt_id = receipt.receipt().receipt_id().as_str();
    connection.execute(
        "INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(receipt_id) DO NOTHING",
        params![
            receipt_id,
            receipt.receipt().sanitizer_version().as_str(),
            receipt
                .payload()
                .map_or("", |payload| payload.digest().as_str()),
            receipt_json,
        ],
    )?;
    let stored_receipt: String = connection.query_row(
        "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
        [receipt_id],
        |row| row.get(0),
    )?;
    if stored_receipt != receipt_json {
        return Err(invalid("sanitization receipt identity collision"));
    }
    Ok(())
}

pub(super) fn cursor_advance_receipt_matches(
    connection: &rusqlite::Connection,
    source_json: &str,
    scope_json: &str,
    advance: &ObservationCursorAdvance,
) -> rusqlite::Result<bool> {
    let stored = connection
        .query_row(
            READ_CURSOR_ADVANCE_SQL,
            params![source_json, scope_json, encode(&advance.coverage())?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let expected_receipt_id = advance
        .sanitization_receipt()
        .map(|receipt| receipt.receipt().receipt_id().as_str());
    if !cursor_advance_ledger_row_matches(
        stored.as_ref(),
        advance.reason().as_str(),
        expected_receipt_id,
    ) {
        return Ok(false);
    }
    if let Some(receipt) = advance.sanitization_receipt() {
        let receipt_json = connection
            .query_row(
                "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
                [receipt.receipt().receipt_id().as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if receipt_json.as_deref() != Some(encode(receipt)?.as_str()) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn persist_retrieval_anchor(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV2,
) -> rusqlite::Result<()> {
    let anchor_json = encode(anchor)?;
    let owner_json = encode(anchor.owner())?;
    let inserted = connection.execute(
        "INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(anchor_id) DO NOTHING",
        params![
            anchor.anchor_id().as_str(),
            anchor_json,
            owner_json,
            anchor.projection_generation().as_str(),
        ],
    )?;
    // A conflict means the anchor was already stored: nothing left to write,
    // and the identity/alias checks are exactly what verification does.
    if inserted == 0 {
        return verify_retrieval_anchor(connection, anchor);
    }
    for alias in anchor.aliases() {
        connection.execute(
            "INSERT INTO retrieval_anchor_aliases (
                owner_json, alias_kind, locator_digest, anchor_id
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner_json, alias_kind, locator_digest) DO NOTHING",
            params![
                owner_json,
                encode(&alias.kind())?,
                encode(alias.locator_digest())?,
                anchor.anchor_id().as_str(),
            ],
        )?;
    }
    // The row we just inserted trivially matches, so verification is really
    // reading back the aliases: any that resolved to a different anchor, or a
    // count that outruns this record's aliases, is a collision.
    verify_retrieval_anchor(connection, anchor)
}

fn verify_retrieval_anchor(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV2,
) -> rusqlite::Result<()> {
    let owner_json = encode(anchor.owner())?;
    let stored = connection
        .query_row(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            [anchor.anchor_id().as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_anchor_json, stored_owner_json, stored_projection_generation)) = stored else {
        return Err(invalid("retrieval anchor identity collision"));
    };
    let stored_anchor: RetrievalAnchorRecordV2 = decode(stored_anchor_json)?;
    if !stored_anchor.is_semantic_replay_of(anchor)
        || stored_owner_json != owner_json
        || stored_projection_generation != anchor.projection_generation().as_str()
    {
        return Err(invalid("retrieval anchor identity collision"));
    }
    let mut owned_aliases = 0_usize;
    for alias in anchor.aliases() {
        let stored_anchor_id = connection
            .query_row(
                "SELECT anchor_id FROM retrieval_anchor_aliases
                 WHERE owner_json = ?1 AND alias_kind = ?2 AND locator_digest = ?3",
                params![
                    owner_json,
                    encode(&alias.kind())?,
                    encode(alias.locator_digest())?,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if stored_anchor_id.as_deref() == Some(anchor.anchor_id().as_str()) {
            owned_aliases += 1;
        } else if !matches!(
            stored_anchor_id.as_deref(),
            Some(current) if cline_alias_transition_is_valid(connection, anchor, current)?
        ) {
            return Err(invalid("retrieval anchor alias collision"));
        }
    }
    let alias_count = connection.query_row(
        "SELECT COUNT(*) FROM retrieval_anchor_aliases
         WHERE owner_json = ?1 AND anchor_id = ?2",
        params![owner_json, anchor.anchor_id().as_str()],
        |row| row.get::<_, i64>(0),
    )?;
    if usize::try_from(alias_count).ok() != Some(owned_aliases) {
        return Err(invalid("retrieval anchor alias collision"));
    }
    Ok(())
}

// Alias promotion belongs to the projector transaction: a newly captured
// successor may wait behind the still-readable predecessor. Once promoted,
// immutable historical anchor replay requires the exact supersession receipt.
fn cline_alias_transition_is_valid(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV2,
    current_anchor_id: &str,
) -> rusqlite::Result<bool> {
    let Some(current_json) = connection
        .query_row(
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = ?1",
            [current_anchor_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let current: RetrievalAnchorRecordV2 = decode(current_json)?;
    let (
        RetrievalAnchorTargetV2::ExactObservation(anchor_observation_id),
        RetrievalAnchorTargetV2::ExactObservation(current_observation_id),
    ) = (anchor.target(), current.target())
    else {
        return Ok(false);
    };
    let anchor_auth = anchor.authorization();
    let current_auth = current.authorization();
    if current.anchor_id().as_str() != current_anchor_id
        || anchor.owner() != current.owner()
        || anchor.aliases() != current.aliases()
        || anchor.payload_access() != current.payload_access()
        || anchor.retention_class() != current.retention_class()
        || anchor.durability() != current.durability()
        || anchor_auth.resolved_scope_id != current_auth.resolved_scope_id
        || anchor_auth.privacy_domain_id != current_auth.privacy_domain_id
        || anchor_auth.access_policy_digest != current_auth.access_policy_digest
        || anchor_auth.capability_id != current_auth.capability_id
    {
        return Ok(false);
    }
    let Some((anchor_json, current_json)) = connection
        .query_row(
            "SELECT candidate.observation_json, current.observation_json
             FROM observations AS candidate
             JOIN observations AS current ON current.observation_id = ?2
             WHERE candidate.observation_id = ?1",
            params![
                anchor_observation_id.as_str(),
                current_observation_id.as_str()
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let candidate_observation: DurableObservationV1 = decode(anchor_json)?;
    let current_observation: DurableObservationV1 = decode(current_json)?;
    if prove_cline_native_source_transition(&current_observation, &candidate_observation).is_some()
    {
        // Pending successor: leave current alias and predecessor availability
        // unchanged until derived output promotion commits.
        return Ok(true);
    }
    if prove_cline_native_source_transition(&candidate_observation, &current_observation).is_none()
    {
        return Ok(false);
    }
    let Some(disposition_json) = connection
        .query_row(
            "SELECT record_json FROM retrieval_anchor_dispositions
             WHERE anchor_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [anchor.anchor_id().as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let disposition: RetrievalAnchorDispositionRecordV1 = decode(disposition_json)?;
    disposition.validate().map_err(invalid)?;
    Ok(disposition.anchor_id() == anchor.anchor_id()
        && disposition.owner().v2() == Some(&FactOwnerV1::from(anchor.owner().clone()))
        && disposition.state() == AnchorDispositionStateV1::Superseded
        && disposition.reason_class() == AnchorDispositionReasonClassV1::Correction
        && disposition.superseded_by() == Some(current.anchor_id()))
}

pub(super) fn persist_repository_provenance(
    connection: &rusqlite::Connection,
    observation_id: &str,
    attachment: &RepositoryProvenanceAttachmentV1,
) -> rusqlite::Result<()> {
    if let Some(anchor) = attachment.anchor() {
        persist_retrieval_anchor(connection, anchor)?;
    }
    connection.execute(
        "INSERT INTO observation_repository_provenance (
            observation_id, availability_json, capture_json, retrieval_anchor_id, owner_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            observation_id,
            encode(attachment.availability())?,
            attachment.provenance().map(encode).transpose()?,
            attachment
                .anchor()
                .map(|anchor| anchor.anchor_id().as_str()),
            attachment
                .anchor()
                .map(|anchor| encode(anchor.owner()))
                .transpose()?,
        ],
    )?;
    Ok(())
}

pub(super) fn verify_observation_authority(
    connection: &rusqlite::Connection,
    write: &AnchoredObservationWrite,
) -> rusqlite::Result<()> {
    let observation_id = write.observation().observation_id().as_str();
    let bound_anchor_id = connection
        .query_row(
            "SELECT anchor_id FROM observation_retrieval_anchors WHERE observation_id = ?1",
            [observation_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if bound_anchor_id.as_deref() != Some(write.retrieval_anchor_id().as_str()) {
        return Err(invalid("observation retrieval anchor collision"));
    }
    verify_retrieval_anchor(connection, write.retrieval_anchor())?;

    let attachment = write.repository_provenance_attachment();
    let stored = connection
        .query_row(
            "SELECT availability_json, capture_json, retrieval_anchor_id, owner_json
             FROM observation_repository_provenance WHERE observation_id = ?1",
            [observation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let expected = (
        encode(attachment.availability())?,
        attachment.provenance().map(encode).transpose()?,
        attachment
            .anchor()
            .map(|anchor| anchor.anchor_id().as_str().to_owned()),
        attachment
            .anchor()
            .map(|anchor| encode(anchor.owner()))
            .transpose()?,
    );
    if stored.as_ref() != Some(&expected) {
        let Some((availability, capture, anchor_id, owner)) = stored else {
            return Err(invalid("observation repository provenance collision"));
        };
        let replay = repository_replay_anchor(attachment, &availability)?;
        let Some(replay) = replay else {
            return Err(invalid("observation repository provenance collision"));
        };
        let retained: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> =
            decode(availability)?;
        if capture != retained.value().map(encode).transpose()?
            || anchor_id.as_deref() != Some(replay.anchor_id().as_str())
            || owner.as_deref() != Some(encode(replay.owner())?.as_str())
        {
            return Err(invalid("observation repository provenance collision"));
        }
        return verify_retrieval_anchor(connection, &replay);
    }
    if let Some(anchor) = attachment.anchor() {
        verify_retrieval_anchor(connection, anchor)?;
    }
    Ok(())
}

/// A concurrent first writer can recapture the same Git evidence at a later
/// local clock. Normalize only that clock and its derived capture identities;
/// the caller still verifies every retained provenance and anchor field and
/// returns the original immutable receipt. Different Git evidence is a conflict.
fn repository_replay_anchor(
    attachment: &RepositoryProvenanceAttachmentV1,
    retained_json: &str,
) -> rusqlite::Result<Option<RetrievalAnchorRecordV2>> {
    let retained: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> =
        decode(retained_json.to_owned())?;
    let (Some(old), Some(new), Some(anchor)) = (
        retained.value(),
        attachment.provenance(),
        attachment.anchor(),
    ) else {
        return Ok(None);
    };
    let capture = new.capture();
    let normalized = GenerationBoundRepositoryProvenanceV1::new(
        new.generation_id().clone(),
        RepositoryProvenanceV1::new(
            capture.repository_id().clone(),
            capture.project_id().cloned(),
            capture.worktree_id().cloned(),
            capture.canonical_root_digest().clone(),
            capture.evidence().clone(),
            old.capture().captured_at(),
        )
        .map_err(invalid)?,
        new.source_observation().cloned(),
    )
    .map_err(invalid)?;
    let normalized = match attachment.availability() {
        EvidenceAvailabilityV1::Known(_) => EvidenceAvailabilityV1::Known(normalized),
        EvidenceAvailabilityV1::PartiallyReadable(_) => {
            EvidenceAvailabilityV1::PartiallyReadable(normalized)
        }
        _ => return Ok(None),
    };
    if encode(&normalized)? != retained_json {
        return Ok(None);
    }
    let RetrievalAnchorTargetV2::RepositoryCapture {
        repository_id,
        receipt,
        ..
    } = anchor.target()
    else {
        return Ok(None);
    };
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::RepositoryCapture {
            repository_id: repository_id.clone(),
            capture_id: old.capture_id().clone(),
            receipt: receipt.clone(),
        },
        owner: anchor.owner().clone(),
        aliases: anchor.aliases().to_vec(),
        occurred_at: anchor.occurred_at(),
        ingested_at: anchor.ingested_at(),
        evidence_class: anchor.evidence_class(),
        source_generation: AnchorSourceGenerationV2::RepositoryCapture(old.capture_id().clone()),
        projection_generation: anchor.projection_generation().clone(),
        projection_watermark: anchor.projection_watermark().clone(),
        coverage: anchor.coverage().clone(),
        source_observations: anchor.source_observations().to_vec(),
        source_anchors: anchor.source_anchors().to_vec(),
        authorization: anchor.authorization().clone(),
        payload_access: anchor.payload_access(),
        retention_class: anchor.retention_class().clone(),
        durability: anchor.durability().clone(),
    })
    .map(Some)
    .map_err(invalid)
}

pub(super) fn read_cursor(
    connection: &rusqlite::Connection,
    source_json: &str,
    scope_json: &str,
) -> rusqlite::Result<Option<ObservationSourceCursorV1>> {
    connection
        .query_row(
            READ_SOURCE_CURSOR_SQL,
            params![source_json, scope_json],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(decode)
        .transpose()
}
