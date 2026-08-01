//! The two evidence assembly read operations and the persistence checks
//! they make before serving a result.

use rusqlite::{OptionalExtension, Transaction, params};
use tracedecay_domain::RetrieverContributionIdV1;
use tracedecay_store::{
    CanonicalSourceOccurrenceSetRecordV1, EvidenceAssemblyDrilldownPageV1,
    EvidenceAssemblyIdempotencyKeyV1, EvidenceAssemblyOwnerV1,
    EvidenceAssemblyPublicationReceiptV1, EvidenceAssemblyReadResultV1,
    EvidenceSourceOccurrenceRecordV1, RetrieverContributionRecordV1,
};

use std::collections::BTreeSet;

use super::super::support::{canonical_digest, decode, invalid, u64_to_i64, usize_to_i64};
use super::anchor_state::{self, evidence_anchor_is_current};

pub(super) fn publication_by_idempotency(
    snapshot: &Transaction<'_>,
    owner: &EvidenceAssemblyOwnerV1,
    idempotency_key: &EvidenceAssemblyIdempotencyKeyV1,
) -> rusqlite::Result<EvidenceAssemblyReadResultV1> {
    let receipt = snapshot
        .query_row(
            "SELECT publication_receipt_id, assembly_digest, occurrence_set_id,
                    span_id, contribution_id, projection_receipt_id, receipt_json
             FROM evidence_assembly_receipts
             WHERE owner_digest = ?1 AND privacy_domain_id = ?2
               AND key_epoch = ?3 AND idempotency_key = ?4",
            params![
                canonical_digest(owner)?,
                owner.owner.privacy_domain_id().as_str(),
                u64_to_i64(owner.key_epoch, "evidence assembly key epoch")?,
                idempotency_key.as_digest().as_str(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?
        .map(
            |(
                publication_receipt_id,
                assembly_digest,
                occurrence_set_id,
                span_id,
                contribution_id,
                projection_receipt_id,
                record,
            )| {
                let receipt: EvidenceAssemblyPublicationReceiptV1 = decode(record)?;
                receipt.validate().map_err(invalid)?;
                let expected_id =
                    tracedecay_store::derive_evidence_assembly_publication_receipt_id_v1(
                        &receipt.identity_projection(idempotency_key.clone()),
                    )
                    .map_err(invalid)?;
                if &receipt.owner != owner
                    || receipt.publication_receipt_id != expected_id
                    || receipt.publication_receipt_id.as_str() != publication_receipt_id
                    || receipt.assembly_digest.as_str() != assembly_digest
                    || receipt.occurrence_set_id.as_str() != occurrence_set_id
                    || receipt.span_id.as_str() != span_id
                    || receipt.contribution_id.as_str() != contribution_id
                    || receipt.projection_receipt_id.as_str() != projection_receipt_id
                {
                    return Err(invalid("evidence publication receipt identity"));
                }
                Ok(receipt)
            },
        )
        .transpose()?;
    Ok(EvidenceAssemblyReadResultV1::Publication(receipt))
}

pub(super) fn contribution_page(
    snapshot: &Transaction<'_>,
    owner: &EvidenceAssemblyOwnerV1,
    contribution_id: &RetrieverContributionIdV1,
    start_ordinal: u64,
    page_size: u64,
) -> rusqlite::Result<EvidenceAssemblyReadResultV1> {
    if page_size == 0 || page_size > 256 {
        return Err(invalid("evidence drilldown page size"));
    }
    let owner_digest = canonical_digest(owner)?;
    let evidence_owner_digest = canonical_digest(&owner.owner)?;
    let Some(contribution) = snapshot
        .query_row(
            "SELECT span_id, anchor_id, record_digest, record_json
             FROM evidence_retriever_contributions
             WHERE contribution_id = ?1 AND owner_digest = ?2",
            params![contribution_id.as_str(), owner_digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .map(|(span_id, anchor_id, record_digest, record_json)| {
            let contribution: RetrieverContributionRecordV1 = decode(record_json)?;
            contribution.validate().map_err(invalid)?;
            if &contribution.contribution_id != contribution_id
                || contribution.span_id.as_str() != span_id
                || contribution.anchor.anchor_id().as_str() != anchor_id
                || canonical_digest(&contribution)? != record_digest
            {
                return Err(invalid(
                    "evidence retriever contribution persistence mismatch",
                ));
            }
            Ok(contribution)
        })
        .transpose()?
    else {
        return Ok(EvidenceAssemblyReadResultV1::ContributionPage(None));
    };
    if &contribution.owner != owner {
        return Ok(EvidenceAssemblyReadResultV1::ContributionPage(None));
    }
    if !evidence_anchor_is_current(snapshot, &contribution.anchor)? {
        return Ok(EvidenceAssemblyReadResultV1::ContributionPage(None));
    }
    let span: tracedecay_store::EvidenceSpanRecordV1 = snapshot
        .query_row(
            "SELECT owner_digest, occurrence_set_id, anchor_id, producer_kind,
                    record_digest, record_json
             FROM evidence_spans WHERE span_id = ?1",
            [contribution.span_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .and_then(
            |(
                stored_owner,
                occurrence_set_id,
                anchor_id,
                producer_kind,
                record_digest,
                record_json,
            )| {
                let span: tracedecay_store::EvidenceSpanRecordV1 = decode(record_json)?;
                span.validate().map_err(invalid)?;
                if stored_owner.as_str() != evidence_owner_digest.as_str()
                    || span.occurrence_set_id.as_str() != occurrence_set_id
                    || span.anchor.anchor_id().as_str() != anchor_id
                    || producer_kind != "v3"
                    || canonical_digest(&span)? != record_digest
                {
                    return Err(invalid("evidence span persistence mismatch"));
                }
                Ok(span)
            },
        )?;
    if span.owner != owner.owner
        || span.span_id != contribution.span_id
        || span.occurrence_set_id != contribution.occurrence_set_id
        || &contribution.span_anchor_id != span.anchor.anchor_id()
        || contribution.exact_source_anchors != span.exact_source_anchors
    {
        return Err(invalid("evidence drilldown cross-record binding"));
    }
    validate_occurrence_set(snapshot, owner, &span)?;
    validate_span_members(snapshot, &span)?;
    if !evidence_anchor_is_current(snapshot, &span.anchor)? {
        return Ok(EvidenceAssemblyReadResultV1::ContributionPage(None));
    }
    let end = start_ordinal.saturating_add(page_size);
    let mut statement = snapshot.prepare(
        "SELECT member.occurrence_id, occurrence.owner_digest,
                occurrence.timeline_digest, occurrence.source_anchor_id,
                occurrence.source_order, occurrence.record_digest,
                occurrence.record_json
         FROM evidence_span_members AS member
         JOIN evidence_source_occurrences AS occurrence
           ON occurrence.occurrence_id = member.occurrence_id
         WHERE member.span_id = ?1
           AND member.assembly_ordinal >= ?2
           AND member.assembly_ordinal < ?3
         ORDER BY member.assembly_ordinal",
    )?;
    let occurrences = statement
        .query_map(
            params![
                span.span_id.as_str(),
                u64_to_i64(start_ordinal, "evidence drilldown start")?,
                u64_to_i64(end, "evidence drilldown end")?,
            ],
            |row| {
                let occurrence_id = row.get::<_, String>(0)?;
                let stored_owner = row.get::<_, String>(1)?;
                let timeline_digest = row.get::<_, String>(2)?;
                let source_anchor_id = row.get::<_, String>(3)?;
                let source_order = row.get::<_, i64>(4)?;
                let record_digest = row.get::<_, String>(5)?;
                let occurrence: EvidenceSourceOccurrenceRecordV1 =
                    row.get::<_, String>(6).and_then(decode)?;
                occurrence.validate().map_err(invalid)?;
                if occurrence.occurrence_id.as_str() != occurrence_id
                    || occurrence.owner != owner.owner
                    || stored_owner.as_str() != evidence_owner_digest.as_str()
                    || occurrence.timeline.digest().map_err(invalid)?.as_str() != timeline_digest
                    || occurrence.exact_source_anchor.as_str() != source_anchor_id
                    || u64_to_i64(occurrence.source_order, "evidence source occurrence order")?
                        != source_order
                    || canonical_digest(&occurrence)? != record_digest
                {
                    return Err(invalid("evidence drilldown occurrence binding"));
                }
                Ok(occurrence)
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let consumed =
        start_ordinal.saturating_add(u64::try_from(occurrences.len()).unwrap_or(u64::MAX));
    let mut anchor_ids = BTreeSet::new();
    for occurrence in &occurrences {
        anchor_ids.insert(occurrence.occurrence_anchor.anchor_id().as_str().to_owned());
        anchor_ids.insert(occurrence.exact_source_anchor.as_str().to_owned());
    }
    let liveness = anchor_state::load_anchor_liveness(snapshot, &anchor_ids)?;
    for occurrence in &occurrences {
        if !liveness.evidence_anchor_is_current(&occurrence.occurrence_anchor)? {
            return Ok(EvidenceAssemblyReadResultV1::ContributionPage(None));
        }
        liveness.require_source_anchor_current(occurrence)?;
    }
    let total = u64::try_from(span.ordered_occurrence_ids().len()).unwrap_or(u64::MAX);
    Ok(EvidenceAssemblyReadResultV1::ContributionPage(Some(
        EvidenceAssemblyDrilldownPageV1 {
            occurrence_set_id: contribution.occurrence_set_id.clone(),
            contribution,
            span,
            occurrences,
            next_ordinal: (consumed < total).then_some(consumed),
        },
    )))
}

fn validate_occurrence_set(
    connection: &rusqlite::Connection,
    owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
    span: &tracedecay_store::EvidenceSpanRecordV1,
) -> rusqlite::Result<()> {
    let (owner_digest, record_digest, record_json) = connection.query_row(
        "SELECT owner_digest, record_digest, record_json
         FROM evidence_occurrence_sets WHERE occurrence_set_id = ?1",
        [span.occurrence_set_id.as_str()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    let occurrence_set: CanonicalSourceOccurrenceSetRecordV1 = decode(record_json)?;
    occurrence_set.validate().map_err(invalid)?;
    if occurrence_set.occurrence_set_id != span.occurrence_set_id
        || occurrence_set.owner != owner.owner
        || owner_digest != canonical_digest(&owner.owner)?
        || record_digest != canonical_digest(&occurrence_set)?
    {
        return Err(invalid("evidence occurrence set persistence mismatch"));
    }
    let mut statement = connection.prepare(
        "SELECT canonical_ordinal, occurrence_id
         FROM evidence_occurrence_set_members
         WHERE occurrence_set_id = ?1
         ORDER BY canonical_ordinal",
    )?;
    let members = statement
        .query_map([span.occurrence_set_id.as_str()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if members.len() != occurrence_set.members.len() {
        return Err(invalid("evidence occurrence set membership mismatch"));
    }
    for (ordinal, ((stored_ordinal, stored_id), expected_id)) in
        members.iter().zip(&occurrence_set.members).enumerate()
    {
        if *stored_ordinal != usize_to_i64(ordinal, "evidence canonical occurrence ordinal")?
            || stored_id != expected_id.as_str()
        {
            return Err(invalid("evidence occurrence set membership mismatch"));
        }
    }
    Ok(())
}

fn validate_span_members(
    connection: &rusqlite::Connection,
    span: &tracedecay_store::EvidenceSpanRecordV1,
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(
        "SELECT assembly_ordinal, run_ordinal, run_member_ordinal, occurrence_id
         FROM evidence_span_members
         WHERE span_id = ?1
         ORDER BY assembly_ordinal",
    )?;
    let members = statement
        .query_map([span.span_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let expected =
        span.runs
            .iter()
            .enumerate()
            .flat_map(|(run_ordinal, run)| {
                run.occurrence_ids.iter().enumerate().map(
                    move |(run_member_ordinal, occurrence_id)| {
                        (run_ordinal, run_member_ordinal, occurrence_id)
                    },
                )
            })
            .collect::<Vec<_>>();
    if members.len() != expected.len() {
        return Err(invalid("evidence span membership mismatch"));
    }
    for (assembly_ordinal, (member, expected)) in members.iter().zip(expected).enumerate() {
        if member.0 != usize_to_i64(assembly_ordinal, "evidence assembly ordinal")?
            || member.1 != usize_to_i64(expected.0, "evidence run ordinal")?
            || member.2 != usize_to_i64(expected.1, "evidence run member ordinal")?
            || member.3 != expected.2.as_str()
        {
            return Err(invalid("evidence span membership mismatch"));
        }
    }
    Ok(())
}
