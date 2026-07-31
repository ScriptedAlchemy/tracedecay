use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{RetrievalAnchorRecordV3, RetrievalAnchorTargetV3};
use tracedecay_store::{
    CanonicalSourceOccurrenceSetRecordV1, EvidenceAssemblyDrilldownPageV1,
    EvidenceAssemblyPublicationReceiptV1, EvidenceAssemblyReadOperationV1,
    EvidenceAssemblyReadResultV1, EvidenceAssemblyWriteV1, EvidenceSourceOccurrenceRecordV1,
    RetrievalAnchorOwnerV1, RetrieverContributionRecordV1,
};

use super::support::{canonical_digest, decode, encode, invalid, u64_to_i64, usize_to_i64};

#[derive(Clone, Default)]
pub struct EvidenceAssemblyExecutor;

impl EvidenceAssemblyExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        write: &EvidenceAssemblyWriteV1,
    ) -> rusqlite::Result<()> {
        write.validate().map_err(invalid)?;
        let owner_digest = canonical_digest(&write.owner)?;
        let evidence_owner_digest = canonical_digest(&write.owner.owner)?;
        if let Some((assembly_digest, receipt_json)) = savepoint
            .query_row(
                "SELECT assembly_digest, receipt_json
                 FROM evidence_assembly_receipts
                 WHERE owner_digest = ?1 AND privacy_domain_id = ?2
                   AND key_epoch = ?3 AND idempotency_key = ?4",
                params![
                    owner_digest,
                    write.owner.owner.privacy_domain_id().as_str(),
                    u64_to_i64(write.owner.key_epoch, "evidence assembly key epoch")?,
                    write.idempotency_key.as_digest().as_str(),
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            let existing =
                decode::<tracedecay_store::EvidenceAssemblyPublicationReceiptV1>(receipt_json)?;
            existing.validate().map_err(invalid)?;
            return if assembly_digest == write.receipt.assembly_digest.as_str()
                && existing == write.receipt
            {
                Ok(())
            } else {
                Err(invalid("evidence assembly replay conflict"))
            };
        }

        for occurrence in &write.occurrences {
            require_source_anchor_current(savepoint, occurrence)?;
            insert_anchor(savepoint, &occurrence.occurrence_anchor)?;
            insert_immutable(
                savepoint,
                "evidence_source_occurrences",
                "occurrence_id",
                occurrence.occurrence_id.as_str(),
                canonical_digest(occurrence)?,
                encode(occurrence)?,
                &[
                    ("owner_digest", evidence_owner_digest.clone()),
                    (
                        "timeline_digest",
                        occurrence.timeline.digest().map_err(invalid)?.to_string(),
                    ),
                    (
                        "source_anchor_id",
                        occurrence.exact_source_anchor.as_str().to_owned(),
                    ),
                    ("source_order", occurrence.source_order.to_string()),
                ],
            )?;
        }

        insert_immutable(
            savepoint,
            "evidence_occurrence_sets",
            "occurrence_set_id",
            write.occurrence_set.occurrence_set_id.as_str(),
            canonical_digest(&write.occurrence_set)?,
            encode(&write.occurrence_set)?,
            &[("owner_digest", evidence_owner_digest.clone())],
        )?;
        for (ordinal, occurrence_id) in write.occurrence_set.members.iter().enumerate() {
            insert_membership(
                savepoint,
                "evidence_occurrence_set_members",
                "occurrence_set_id",
                write.occurrence_set.occurrence_set_id.as_str(),
                "canonical_ordinal",
                ordinal,
                occurrence_id.as_str(),
            )?;
        }

        insert_anchor(savepoint, &write.span.anchor)?;
        insert_immutable(
            savepoint,
            "evidence_spans",
            "span_id",
            write.span.span_id.as_str(),
            canonical_digest(&write.span)?,
            encode(&write.span)?,
            &[
                ("owner_digest", evidence_owner_digest.clone()),
                (
                    "occurrence_set_id",
                    write.occurrence_set.occurrence_set_id.as_str().to_owned(),
                ),
                (
                    "anchor_id",
                    write.span.anchor.anchor_id().as_str().to_owned(),
                ),
                ("producer_kind", "v3".to_owned()),
            ],
        )?;
        let mut assembly_ordinal = 0;
        for (run_ordinal, run) in write.span.runs.iter().enumerate() {
            for (member_ordinal, occurrence_id) in run.occurrence_ids.iter().enumerate() {
                insert_span_membership(
                    savepoint,
                    write.span.span_id.as_str(),
                    assembly_ordinal,
                    run_ordinal,
                    member_ordinal,
                    occurrence_id.as_str(),
                )?;
                assembly_ordinal = assembly_ordinal
                    .checked_add(1)
                    .ok_or_else(|| invalid("evidence span assembly ordinal overflow"))?;
            }
        }

        insert_immutable(
            savepoint,
            "evidence_span_projection_receipts",
            "projection_receipt_id",
            write.projection_receipt.projection_receipt_id.as_str(),
            canonical_digest(&write.projection_receipt)?,
            encode(&write.projection_receipt)?,
            &[("span_id", write.span.span_id.as_str().to_owned())],
        )?;

        insert_anchor(savepoint, &write.contribution.anchor)?;
        insert_immutable(
            savepoint,
            "evidence_retriever_contributions",
            "contribution_id",
            write.contribution.contribution_id.as_str(),
            canonical_digest(&write.contribution)?,
            encode(&write.contribution)?,
            &[
                ("owner_digest", owner_digest.clone()),
                ("span_id", write.span.span_id.as_str().to_owned()),
                (
                    "anchor_id",
                    write.contribution.anchor.anchor_id().as_str().to_owned(),
                ),
            ],
        )?;

        for anchor in [&write.span.anchor, &write.contribution.anchor]
            .into_iter()
            .chain(
                write
                    .occurrences
                    .iter()
                    .map(|occurrence| &occurrence.occurrence_anchor),
            )
        {
            insert_derived_anchor(savepoint, anchor, &evidence_owner_digest)?;
        }

        publish_reverse_lineage(savepoint, write)?;
        savepoint.execute(
            "INSERT INTO evidence_assembly_receipts (
                publication_receipt_id, owner_digest, privacy_domain_id, key_epoch,
                idempotency_key, assembly_digest, occurrence_set_id, span_id,
                contribution_id, projection_receipt_id, receipt_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                write.receipt.publication_receipt_id.as_str(),
                owner_digest,
                write.owner.owner.privacy_domain_id().as_str(),
                u64_to_i64(write.owner.key_epoch, "evidence assembly key epoch")?,
                write.idempotency_key.as_digest().as_str(),
                write.receipt.assembly_digest.as_str(),
                write.occurrence_set.occurrence_set_id.as_str(),
                write.span.span_id.as_str(),
                write.contribution.contribution_id.as_str(),
                write.projection_receipt.projection_receipt_id.as_str(),
                encode(&write.receipt)?,
            ],
        )?;
        Ok(())
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &EvidenceAssemblyReadOperationV1,
    ) -> rusqlite::Result<EvidenceAssemblyReadResultV1> {
        match operation {
            EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                owner,
                idempotency_key,
            } => {
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
            EvidenceAssemblyReadOperationV1::ContributionPage {
                owner,
                contribution_id,
                start_ordinal,
                page_size,
            } => {
                if *page_size == 0 || *page_size > 256 {
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
                let end = start_ordinal.saturating_add(*page_size);
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
                            u64_to_i64(*start_ordinal, "evidence drilldown start")?,
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
                                || occurrence.timeline.digest().map_err(invalid)?.as_str()
                                    != timeline_digest
                                || occurrence.exact_source_anchor.as_str() != source_anchor_id
                                || u64_to_i64(
                                    occurrence.source_order,
                                    "evidence source occurrence order",
                                )? != source_order
                                || canonical_digest(&occurrence)? != record_digest
                            {
                                return Err(invalid("evidence drilldown occurrence binding"));
                            }
                            Ok(occurrence)
                        },
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let consumed = start_ordinal
                    .saturating_add(u64::try_from(occurrences.len()).unwrap_or(u64::MAX));
                for occurrence in &occurrences {
                    if !evidence_anchor_is_current(snapshot, &occurrence.occurrence_anchor)? {
                        return Ok(EvidenceAssemblyReadResultV1::ContributionPage(None));
                    }
                    require_source_anchor_current(snapshot, occurrence)?;
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
        }
    }
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

fn evidence_anchor_is_current(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV3,
) -> rusqlite::Result<bool> {
    let owner_json = encode(anchor.owner())?;
    let Some((anchor_json, projection_generation)) = connection
        .query_row(
            "SELECT anchor_json, projection_generation
             FROM retrieval_anchors
             WHERE anchor_id = ?1 AND owner_json = ?2",
            params![anchor.anchor_id().as_str(), owner_json],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    if anchor_json != encode(anchor)?
        || projection_generation != anchor.projection_generation().as_str()
    {
        return Err(invalid("evidence retrieval anchor persistence mismatch"));
    }
    let state = latest_disposition_state(connection, anchor.anchor_id().as_str(), &owner_json)?;
    Ok(state.as_deref().is_none_or(|state| state == "active"))
}

/// Reads the newest disposition recorded for an anchor, if it has one.
///
/// `None` means the anchor was never disposed, which every caller treats the
/// same as an explicitly active disposition.
fn latest_disposition_state(
    connection: &rusqlite::Connection,
    anchor_id: &str,
    owner_json: &str,
) -> rusqlite::Result<Option<String>> {
    connection
        .query_row(
            "SELECT state FROM retrieval_anchor_dispositions
             WHERE anchor_id = ?1 AND owner_json = ?2
             ORDER BY sequence DESC LIMIT 1",
            params![anchor_id, owner_json],
            |row| row.get::<_, String>(0),
        )
        .optional()
}

fn require_source_anchor_current(
    connection: &rusqlite::Connection,
    occurrence: &EvidenceSourceOccurrenceRecordV1,
) -> rusqlite::Result<()> {
    let owner_json = connection
        .query_row(
            "SELECT owner_json FROM retrieval_anchors WHERE anchor_id = ?1",
            [occurrence.exact_source_anchor.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| invalid("evidence source anchor unavailable"))?;
    let source_owner: RetrievalAnchorOwnerV1 = decode(owner_json.clone())?;
    if !source_owner_matches_assembly(&source_owner, &occurrence.owner) {
        return Err(invalid("evidence source anchor owner mismatch"));
    }
    let state = latest_disposition_state(
        connection,
        occurrence.exact_source_anchor.as_str(),
        &owner_json,
    )?;
    if state.as_deref().is_none_or(|state| state == "active") {
        Ok(())
    } else {
        Err(invalid("evidence source anchor is disposed"))
    }
}

fn source_owner_matches_assembly(
    source: &RetrievalAnchorOwnerV1,
    assembly: &tracedecay_domain::AnchorOwnerBindingV1,
) -> bool {
    match source {
        RetrievalAnchorOwnerV1::V3(owner) => owner == assembly,
        // A V2 owner has no authoritative profile/privacy-domain identity.
        // Decoding remains supported, but it cannot establish V3 evidence
        // ownership without a separate exact migration binding.
        RetrievalAnchorOwnerV1::V2(_) => false,
    }
}

fn insert_anchor(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV3,
) -> rusqlite::Result<()> {
    anchor.validate().map_err(invalid)?;
    let anchor_json = encode(anchor)?;
    let owner_json = encode(anchor.owner())?;
    let projection_generation = anchor.projection_generation().as_str();
    let changed = connection.execute(
        "INSERT OR IGNORE INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            anchor.anchor_id().as_str(),
            anchor_json,
            owner_json,
            projection_generation,
        ],
    )?;
    if changed == 1 {
        return Ok(());
    }
    let existing = connection.query_row(
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
    )?;
    if existing == (anchor_json, owner_json, projection_generation.to_owned()) {
        Ok(())
    } else {
        Err(invalid("retrieval anchor replay conflict"))
    }
}

fn insert_immutable(
    connection: &rusqlite::Connection,
    table: &'static str,
    id_column: &'static str,
    id: &str,
    record_digest: String,
    record_json: String,
    extra: &[(&'static str, String)],
) -> rusqlite::Result<()> {
    let mut columns = vec![id_column, "record_digest", "record_json"];
    columns.extend(extra.iter().map(|(column, _)| *column));
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO {table} ({}) VALUES ({placeholders})",
        columns.join(", ")
    );
    let mut values = vec![id.to_owned(), record_digest.clone(), record_json.clone()];
    values.extend(extra.iter().map(|(_, value)| value.clone()));
    let changed = connection.execute(&sql, rusqlite::params_from_iter(values))?;
    if changed == 1 {
        return Ok(());
    }
    let projected_extra = extra
        .iter()
        .map(|(column, _)| format!("CAST({column} AS TEXT)"))
        .collect::<Vec<_>>();
    let extra_projection = if projected_extra.is_empty() {
        String::new()
    } else {
        format!(", {}", projected_extra.join(", "))
    };
    let sql = format!(
        "SELECT record_digest, record_json{extra_projection}
         FROM {table} WHERE {id_column} = ?1"
    );
    let existing = connection.query_row(&sql, [id], |row| {
        (0..(2 + extra.len()))
            .map(|index| row.get::<_, String>(index))
            .collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let mut expected = vec![record_digest, record_json];
    expected.extend(extra.iter().map(|(_, value)| value.clone()));
    if existing == expected {
        Ok(())
    } else {
        Err(invalid(format!("{table} immutable replay conflict")))
    }
}

fn insert_membership(
    connection: &rusqlite::Connection,
    table: &'static str,
    parent_column: &'static str,
    parent_id: &str,
    ordinal_column: &'static str,
    ordinal: usize,
    occurrence_id: &str,
) -> rusqlite::Result<()> {
    let sql = format!(
        "INSERT OR IGNORE INTO {table} (
            {parent_column}, {ordinal_column}, occurrence_id
         ) VALUES (?1, ?2, ?3)"
    );
    let changed = connection.execute(
        &sql,
        params![
            parent_id,
            usize_to_i64(ordinal, "evidence membership ordinal")?,
            occurrence_id
        ],
    )?;
    if changed == 1 {
        return Ok(());
    }
    let sql = format!(
        "SELECT occurrence_id FROM {table}
         WHERE {parent_column} = ?1 AND {ordinal_column} = ?2"
    );
    let existing = connection.query_row(
        &sql,
        params![
            parent_id,
            usize_to_i64(ordinal, "evidence membership ordinal")?
        ],
        |row| row.get::<_, String>(0),
    )?;
    if existing == occurrence_id {
        Ok(())
    } else {
        Err(invalid(format!("{table} immutable replay conflict")))
    }
}

fn insert_span_membership(
    connection: &rusqlite::Connection,
    span_id: &str,
    assembly_ordinal: usize,
    run_ordinal: usize,
    run_member_ordinal: usize,
    occurrence_id: &str,
) -> rusqlite::Result<()> {
    let changed = connection.execute(
        "INSERT OR IGNORE INTO evidence_span_members (
            span_id, assembly_ordinal, run_ordinal, run_member_ordinal, occurrence_id
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            span_id,
            usize_to_i64(assembly_ordinal, "evidence assembly ordinal")?,
            usize_to_i64(run_ordinal, "evidence run ordinal")?,
            usize_to_i64(run_member_ordinal, "evidence run member ordinal")?,
            occurrence_id,
        ],
    )?;
    if changed == 1 {
        return Ok(());
    }
    let existing = connection.query_row(
        "SELECT run_ordinal, run_member_ordinal, occurrence_id
         FROM evidence_span_members
         WHERE span_id = ?1 AND assembly_ordinal = ?2",
        params![
            span_id,
            usize_to_i64(assembly_ordinal, "evidence assembly ordinal")?
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    if existing
        == (
            usize_to_i64(run_ordinal, "evidence run ordinal")?,
            usize_to_i64(run_member_ordinal, "evidence run member ordinal")?,
            occurrence_id.to_owned(),
        )
    {
        Ok(())
    } else {
        Err(invalid("evidence span membership replay conflict"))
    }
}

fn publish_reverse_lineage(
    connection: &rusqlite::Connection,
    write: &EvidenceAssemblyWriteV1,
) -> rusqlite::Result<()> {
    for occurrence in &write.occurrences {
        let owner_json = connection.query_row(
            "SELECT owner_json FROM retrieval_anchors WHERE anchor_id = ?1",
            [occurrence.exact_source_anchor.as_str()],
            |row| row.get::<_, String>(0),
        )?;
        for (kind, derivative_id) in [
            ("span", write.span.span_id.as_str()),
            ("contribution", write.contribution.contribution_id.as_str()),
        ] {
            let changed = connection.execute(
                "INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
                    source_anchor_id, owner_json, derivative_kind, derivative_id,
                    direct_evidence
                 ) VALUES (?1, ?2, ?3, ?4, 1)",
                params![
                    occurrence.exact_source_anchor.as_str(),
                    owner_json,
                    kind,
                    derivative_id,
                ],
            )?;
            if changed == 0 {
                let direct = connection.query_row(
                    "SELECT direct_evidence FROM retrieval_anchor_reverse_lineage
                     WHERE source_anchor_id = ?1 AND owner_json = ?2
                       AND derivative_kind = ?3 AND derivative_id = ?4",
                    params![
                        occurrence.exact_source_anchor.as_str(),
                        owner_json,
                        kind,
                        derivative_id,
                    ],
                    |row| row.get::<_, i64>(0),
                )?;
                if direct != 1 {
                    return Err(invalid("evidence reverse lineage replay conflict"));
                }
            }
        }
    }
    Ok(())
}

fn insert_derived_anchor(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV3,
    owner_digest: &str,
) -> rusqlite::Result<()> {
    let (target_kind, target_id) = evidence_target(anchor)?;
    let anchor_json = encode(anchor)?;
    let changed = connection.execute(
        "INSERT OR IGNORE INTO evidence_derived_anchors (
            anchor_id, owner_digest, target_kind, target_id, anchor_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            anchor.anchor_id().as_str(),
            owner_digest,
            target_kind,
            target_id,
            anchor_json,
        ],
    )?;
    if changed == 1 {
        return Ok(());
    }
    let existing = connection.query_row(
        "SELECT owner_digest, target_kind, target_id, anchor_json
         FROM evidence_derived_anchors WHERE anchor_id = ?1",
        [anchor.anchor_id().as_str()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    if existing
        == (
            owner_digest.to_owned(),
            target_kind.to_owned(),
            target_id.to_owned(),
            anchor_json,
        )
    {
        Ok(())
    } else {
        Err(invalid("evidence derived anchor replay conflict"))
    }
}

fn evidence_target(anchor: &RetrievalAnchorRecordV3) -> rusqlite::Result<(&'static str, &str)> {
    match anchor.target() {
        RetrievalAnchorTargetV3::ExactSourceOccurrence(id) => {
            Ok(("source_occurrence", id.as_str()))
        }
        RetrievalAnchorTargetV3::ExactEvidenceSpan(id) => Ok(("evidence_span", id.as_str())),
        RetrievalAnchorTargetV3::RetrieverContribution(id) => {
            Ok(("retriever_contribution", id.as_str()))
        }
        _ => Err(invalid("non-evidence target in evidence assembly")),
    }
}

#[cfg(any(test, feature = "test-transport"))]
pub mod tests {
    use super::*;
    use tracedecay_domain::{
        AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV3, AnchorOwnerBindingV1,
        AnchorProvenanceRelationV2, AnchorSourceGenerationV3, CoverageReportV1,
        EvidenceAssemblyPublicationReceiptIdV1, EvidenceClass, ManifestDigest,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
        ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadAccessState,
        PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProjectionGenerationId,
        ProviderId, ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId,
        RetrievalAnchorRecordV3Parts, SanitizationReceiptId, SanitizationReceiptRefV1,
        ScopeResolutionId, SessionId, UserProfileId, UtcMicros, VectorWatermark,
    };
    use tracedecay_store::{
        CanonicalSourceOccurrenceSetIdentityProjectionV1, CanonicalSourceOccurrenceSetRecordV1,
        EvidenceAssemblyIdempotencyKeyV1, EvidenceAssemblyOwnerV1,
        EvidenceAssemblyPublicationReceiptV1, EvidenceSourceTimelineV1,
        EvidenceSpanCatalogBindingV1, EvidenceSpanHorizonV1, EvidenceSpanIdentityProjectionV1,
        EvidenceSpanMemberReceiptBindingV1, EvidenceSpanProjectionReceiptIdentityProjectionV1,
        EvidenceSpanProjectionReceiptV1, EvidenceSpanRecordV1, EvidenceSpanRunV1,
        PrivacyBoundRequestDigestV1, PrivacyBoundRequestEnvelopeV1,
        RetrieverContributionIdentityProjectionV1, RetrieverIdentityV1,
        RetrieverWatermarkBindingV1, SanitizedObservationByteRangeV1,
        SourceCapabilityCatalogBindingV1, SourceOccurrenceCoordinateV1,
        SourceOccurrenceIdentityProjectionV1, SourceOccurrenceKindV1,
        SourceOccurrenceSanitizationV1, VerifiedSourceOrderingProofV1,
        derive_canonical_source_occurrence_set_id_v1,
        derive_evidence_assembly_publication_receipt_id_v1, derive_evidence_span_id_v1,
        derive_evidence_span_projection_receipt_id_v1, derive_retriever_contribution_id_v1,
        derive_source_occurrence_id_v1,
    };
    #[cfg(test)]
    use tracedecay_store::{
        RetrievalAnchorReadOperationV1, RetrievalAnchorReadResultV1, StoredRetrievalAnchorRecordV1,
    };

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn owner(project_id: ProjectId) -> EvidenceAssemblyOwnerV1 {
        EvidenceAssemblyOwnerV1 {
            owner: AnchorOwnerBindingV1::for_project(
                UserProfileId::new("profile.fixture").unwrap(),
                project_id,
                PrivacyDomainId::new("privacy.fixture").unwrap(),
            )
            .unwrap(),
            scope_digest: ManifestDigest::new(DIGEST).unwrap(),
            key_epoch: 1,
        }
    }

    fn timeline(project_id: ProjectId) -> EvidenceSourceTimelineV1 {
        EvidenceSourceTimelineV1 {
            source: ObservationSourceIdentityV1::for_provider(
                ProviderId::new("provider.fixture").unwrap(),
                SessionId::new("session.fixture").unwrap(),
            )
            .unwrap(),
            scope: ObservationScopeV1::Project { project_id },
            source_generation: ObservationSourceGenerationV1::new(1).unwrap(),
            ordering_domain: ObservationOrderingDomainV1::DaemonSequence,
        }
    }

    fn catalog_binding() -> SourceCapabilityCatalogBindingV1 {
        SourceCapabilityCatalogBindingV1 {
            connector_id: "connector.fixture".to_owned(),
            root_id: "root.fixture".to_owned(),
            capability_id: tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
            catalog_digest: ManifestDigest::new(DIGEST).unwrap(),
            integration_manifest_digest: ManifestDigest::new(DIGEST).unwrap(),
            configuration_digest: ManifestDigest::new(DIGEST).unwrap(),
            authorization_scope_digest: ManifestDigest::new(DIGEST).unwrap(),
            projector_revision: tracedecay_domain::ComponentVersion::new("projector.fixture")
                .unwrap(),
            source_watermark: ManifestDigest::new(DIGEST).unwrap(),
        }
    }

    fn sanitization() -> SourceOccurrenceSanitizationV1 {
        SourceOccurrenceSanitizationV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.capture.fixture").unwrap(),
                tracedecay_domain::ComponentVersion::new("sanitizer.fixture").unwrap(),
            )
            .unwrap(),
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.projection.fixture").unwrap(),
                tracedecay_domain::ComponentVersion::new("sanitizer.fixture").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn anchor(
        target: RetrievalAnchorTargetV3,
        owner: &EvidenceAssemblyOwnerV1,
        sources: Vec<RetrievalAnchorId>,
    ) -> RetrievalAnchorRecordV3 {
        let source_anchors = sources
            .into_iter()
            .enumerate()
            .map(|(ordinal, source)| {
                AnchorLineageRefV3::new(
                    u64::try_from(ordinal).unwrap(),
                    AnchorProvenanceRelationV2::DerivedFrom,
                    source,
                    owner.owner.clone(),
                )
                .unwrap()
            })
            .collect();
        RetrievalAnchorRecordV3::new(RetrievalAnchorRecordV3Parts {
            target,
            owner: owner.owner.clone(),
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(1),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV3::Unknown,
            projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations: vec![],
            source_anchors,
            authorization: ResolutionAuthorizationV1 {
                resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
                privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
                access_policy_digest: AccessPolicyDigest::new(DIGEST).unwrap(),
                capability_id: tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
                canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST).unwrap(),
            },
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    pub fn write_fixture_for_project(
        component_version: &str,
        project_id: ProjectId,
    ) -> (RetrievalAnchorRecordV3, EvidenceAssemblyWriteV1) {
        let owner = owner(project_id.clone());
        let timeline = timeline(project_id);
        let source_anchor = anchor(
            RetrievalAnchorTargetV3::Entity(tracedecay_domain::EntityRef {
                id: tracedecay_domain::EntityId::new("entity.source.fixture".to_owned()).unwrap(),
                kind: tracedecay_domain::EntityKind::Document,
            }),
            &owner,
            Vec::new(),
        );
        let source = source_anchor.anchor_id().clone();
        let coordinate = SourceOccurrenceCoordinateV1::ObservationProjection {
            canonical_observation_id: tracedecay_domain::CanonicalObservationIdV1::new(format!(
                "sha256:{}",
                "33".repeat(32)
            ))
            .unwrap(),
            source_range: ObservationSourceRangeV1::new(7, 8).unwrap(),
            projection_output_ordinal: 0,
            sanitized_byte_range: SanitizedObservationByteRangeV1::new(0, 8).unwrap(),
        };
        let occurrence_id = derive_source_occurrence_id_v1(&SourceOccurrenceIdentityProjectionV1 {
            owner: owner.owner.clone(),
            timeline: timeline.clone(),
            exact_source_anchor: source.clone(),
            source_order: 7,
            coordinate: coordinate.clone(),
            occurrence_kind: SourceOccurrenceKindV1::Message,
            relations: Vec::new(),
            projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture")
                .unwrap(),
        })
        .unwrap();
        let occurrence_anchor = anchor(
            RetrievalAnchorTargetV3::ExactSourceOccurrence(occurrence_id.clone()),
            &owner,
            vec![source.clone()],
        );
        let occurrence = EvidenceSourceOccurrenceRecordV1 {
            occurrence_id: occurrence_id.clone(),
            owner: owner.owner.clone(),
            timeline,
            exact_source_anchor: source.clone(),
            occurrence_anchor: occurrence_anchor.clone(),
            source_order: 7,
            coordinate,
            occurrence_kind: SourceOccurrenceKindV1::Message,
            relations: Vec::new(),
            projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture")
                .unwrap(),
            sanitization: sanitization(),
            knowledge_time: UtcMicros(1),
            valid_time: Some(UtcMicros(1)),
        };
        let occurrence_set_id = derive_canonical_source_occurrence_set_id_v1(
            &CanonicalSourceOccurrenceSetIdentityProjectionV1 {
                owner: owner.owner.clone(),
                canonical_members: vec![occurrence_id.clone()],
            },
        )
        .unwrap();
        let run = EvidenceSpanRunV1 {
            assembly_ordinal: 0,
            timeline: occurrence.timeline.clone(),
            ordering_proof: VerifiedSourceOrderingProofV1::verify(
                occurrence.timeline.clone(),
                catalog_binding(),
                catalog_binding(),
                vec![occurrence_id.clone()],
                vec![7],
            )
            .unwrap(),
            timeline_digest: occurrence.timeline.digest().unwrap(),
            first_source_order: 7,
            last_source_order: 7,
            occurrence_ids: vec![occurrence_id.clone()],
        };
        let span_id = derive_evidence_span_id_v1(&EvidenceSpanIdentityProjectionV1 {
            owner: owner.owner.clone(),
            occurrence_set_id: occurrence_set_id.clone(),
            ordered_runs: vec![run.clone()],
            exact_source_anchors: vec![source.clone()],
            projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture")
                .unwrap(),
            horizon: EvidenceSpanHorizonV1 {
                knowledge_through: UtcMicros(1),
                valid_through: Some(UtcMicros(1)),
                contains_unknown_valid_time: false,
            },
            catalog_binding: EvidenceSpanCatalogBindingV1::SourceCapability {
                binding: catalog_binding(),
            },
        })
        .unwrap();
        let span_anchor = anchor(
            RetrievalAnchorTargetV3::ExactEvidenceSpan(span_id.clone()),
            &owner,
            vec![occurrence_anchor.anchor_id().clone()],
        );
        let span = EvidenceSpanRecordV1 {
            span_id: span_id.clone(),
            anchor: span_anchor.clone(),
            owner: owner.owner.clone(),
            occurrence_set_id: occurrence_set_id.clone(),
            runs: vec![run],
            exact_source_anchors: vec![source.clone()],
            projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture")
                .unwrap(),
            horizon: EvidenceSpanHorizonV1 {
                knowledge_through: UtcMicros(1),
                valid_through: Some(UtcMicros(1)),
                contains_unknown_valid_time: false,
            },
            catalog_binding: EvidenceSpanCatalogBindingV1::SourceCapability {
                binding: catalog_binding(),
            },
        };
        let member_receipts = vec![EvidenceSpanMemberReceiptBindingV1 {
            occurrence_id: occurrence_id.clone(),
            sanitization: sanitization(),
        }];
        let projection_receipt_id = derive_evidence_span_projection_receipt_id_v1(
            &EvidenceSpanProjectionReceiptIdentityProjectionV1 {
                span_id: span_id.clone(),
                projector_snapshot: "projector.snapshot.fixture".to_owned(),
                projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
                projection_watermark: VectorWatermark::default(),
                source_watermark: ManifestDigest::new(DIGEST).unwrap(),
                member_receipts: member_receipts.clone(),
                ordered_occurrence_ids: vec![occurrence_id.clone()],
                exact_source_anchors: vec![source.clone()],
            },
        )
        .unwrap();
        let horizon = EvidenceSpanHorizonV1 {
            knowledge_through: UtcMicros(1),
            valid_through: Some(UtcMicros(1)),
            contains_unknown_valid_time: false,
        };
        let request_digest = PrivacyBoundRequestDigestV1::derive(
            owner.owner.privacy_domain_id().clone(),
            owner.key_epoch,
            b"fixture-privacy-key",
            &PrivacyBoundRequestEnvelopeV1 {
                use_case_id: tracedecay_domain::UseCaseId::new("use-case.fixture").unwrap(),
                scope_resolution_id: ScopeResolutionId::new("scope.fixture").unwrap(),
                temporal_mode: tracedecay_domain::TemporalModeV1::Current,
                horizon: horizon.clone(),
                requested_capabilities: vec![
                    tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
                ],
            },
        )
        .unwrap();
        let retriever = RetrieverIdentityV1 {
            capability_id: tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
            component_version: tracedecay_domain::ComponentVersion::new(component_version).unwrap(),
        };
        let watermarks = RetrieverWatermarkBindingV1 {
            source_watermark: ManifestDigest::new(DIGEST).unwrap(),
            projection_watermark: VectorWatermark::default(),
            index_watermark: None,
            summary_watermark: None,
        };
        let contribution_id =
            derive_retriever_contribution_id_v1(&RetrieverContributionIdentityProjectionV1 {
                owner: owner.clone(),
                retriever: retriever.clone(),
                catalog_binding: catalog_binding(),
                request_digest: request_digest.clone(),
                scope_resolution_id: ScopeResolutionId::new("scope.fixture").unwrap(),
                temporal_mode: tracedecay_domain::TemporalModeV1::Current,
                watermarks: watermarks.clone(),
                horizon: horizon.clone(),
                occurrence_set_id: occurrence_set_id.clone(),
                span_id: span_id.clone(),
                span_anchor_id: span_anchor.anchor_id().clone(),
                exact_source_anchors: vec![source.clone()],
                coverage: CoverageReportV1::default(),
            })
            .unwrap();
        let contribution_anchor = anchor(
            RetrievalAnchorTargetV3::RetrieverContribution(contribution_id.clone()),
            &owner,
            vec![span_anchor.anchor_id().clone()],
        );
        let mut write = EvidenceAssemblyWriteV1 {
            owner: owner.clone(),
            idempotency_key: EvidenceAssemblyIdempotencyKeyV1::new(
                ManifestDigest::new(format!("sha256:{}", "cc".repeat(32))).unwrap(),
            )
            .unwrap(),
            occurrences: vec![occurrence],
            occurrence_set: CanonicalSourceOccurrenceSetRecordV1 {
                occurrence_set_id: occurrence_set_id.clone(),
                owner: owner.owner.clone(),
                members: vec![occurrence_id.clone()],
            },
            span,
            projection_receipt: EvidenceSpanProjectionReceiptV1 {
                projection_receipt_id: projection_receipt_id.clone(),
                span_id: span_id.clone(),
                projector_snapshot: "projector.snapshot.fixture".to_owned(),
                projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
                projection_watermark: VectorWatermark::default(),
                source_watermark: ManifestDigest::new(DIGEST).unwrap(),
                member_receipts,
                ordered_occurrence_ids: vec![occurrence_id.clone()],
                exact_source_anchors: vec![source.clone()],
            },
            contribution: RetrieverContributionRecordV1 {
                contribution_id: contribution_id.clone(),
                anchor: contribution_anchor.clone(),
                owner: owner.clone(),
                retriever,
                catalog_binding: catalog_binding(),
                request_digest,
                scope_resolution_id: ScopeResolutionId::new("scope.fixture").unwrap(),
                temporal_mode: tracedecay_domain::TemporalModeV1::Current,
                watermarks,
                horizon,
                occurrence_set_id: occurrence_set_id.clone(),
                span_id: span_id.clone(),
                span_anchor_id: span_anchor.anchor_id().clone(),
                exact_source_anchors: vec![source.clone()],
                coverage: CoverageReportV1::default(),
                created_at: UtcMicros(2),
            },
            receipt: EvidenceAssemblyPublicationReceiptV1 {
                publication_receipt_id: EvidenceAssemblyPublicationReceiptIdV1::new(
                    "publication.fixture",
                )
                .unwrap(),
                owner,
                assembly_digest: ManifestDigest::new(DIGEST).unwrap(),
                occurrence_set_id,
                span_id,
                span_anchor_id: span_anchor.anchor_id().clone(),
                contribution_id,
                contribution_anchor_id: contribution_anchor.anchor_id().clone(),
                projection_receipt_id,
                ordered_occurrence_ids: vec![occurrence_id],
                exact_source_anchors: vec![source],
            },
        };
        write.receipt.assembly_digest = write.compute_assembly_digest().unwrap();
        write.receipt.publication_receipt_id = derive_evidence_assembly_publication_receipt_id_v1(
            &write
                .receipt
                .identity_projection(write.idempotency_key.clone()),
        )
        .unwrap();
        write.validate().unwrap();
        (source_anchor, write)
    }

    #[cfg(test)]
    pub(crate) fn write_fixture(component_version: &str) -> EvidenceAssemblyWriteV1 {
        write_fixture_for_project(
            component_version,
            ProjectId::new("project.fixture").unwrap(),
        )
        .1
    }

    #[cfg(test)]
    fn install(connection: &rusqlite::Connection) {
        connection
            .execute_batch(
                "CREATE TABLE retrieval_anchors (
                    anchor_id TEXT PRIMARY KEY, anchor_json TEXT NOT NULL,
                    owner_json TEXT NOT NULL, projection_generation TEXT NOT NULL
                 );
                 CREATE UNIQUE INDEX idx_retrieval_anchors_owner
                   ON retrieval_anchors(anchor_id, owner_json);
                 CREATE TABLE retrieval_anchor_dispositions (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT, disposition_id TEXT UNIQUE,
                    anchor_id TEXT, owner_json TEXT, state TEXT, superseded_by TEXT,
                    reason_class TEXT, effective_at INTEGER, record_json TEXT
                 );
                 CREATE TABLE retrieval_anchor_reverse_lineage (
                    source_anchor_id TEXT, owner_json TEXT, derivative_kind TEXT,
                    derivative_id TEXT, direct_evidence INTEGER,
                    PRIMARY KEY(source_anchor_id, owner_json, derivative_kind, derivative_id)
                 );
                 CREATE TABLE evidence_source_occurrences (
                    occurrence_id TEXT PRIMARY KEY, owner_digest TEXT, timeline_digest TEXT,
                    source_anchor_id TEXT, source_order INTEGER, record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_occurrence_sets (
                    occurrence_set_id TEXT PRIMARY KEY, owner_digest TEXT,
                    record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_occurrence_set_members (
                    occurrence_set_id TEXT, canonical_ordinal INTEGER, occurrence_id TEXT,
                    PRIMARY KEY(occurrence_set_id, canonical_ordinal)
                 );
                 CREATE TABLE evidence_spans (
                    span_id TEXT PRIMARY KEY, owner_digest TEXT, occurrence_set_id TEXT,
                    anchor_id TEXT, producer_kind TEXT, record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_span_members (
                    span_id TEXT, assembly_ordinal INTEGER, run_ordinal INTEGER,
                    run_member_ordinal INTEGER, occurrence_id TEXT,
                    PRIMARY KEY(span_id, assembly_ordinal)
                 );
                 CREATE TABLE evidence_span_projection_receipts (
                    projection_receipt_id TEXT PRIMARY KEY, span_id TEXT,
                    record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_retriever_contributions (
                    contribution_id TEXT PRIMARY KEY, owner_digest TEXT, span_id TEXT,
                    anchor_id TEXT, record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_derived_anchors (
                    anchor_id TEXT PRIMARY KEY, owner_digest TEXT, target_kind TEXT,
                    target_id TEXT, anchor_json TEXT
                 );
                 CREATE TABLE evidence_assembly_receipts (
                    publication_receipt_id TEXT PRIMARY KEY, owner_digest TEXT,
                    privacy_domain_id TEXT, key_epoch INTEGER, idempotency_key TEXT,
                    assembly_digest TEXT, occurrence_set_id TEXT, span_id TEXT,
                    contribution_id TEXT, projection_receipt_id TEXT, receipt_json TEXT,
                    UNIQUE(owner_digest, privacy_domain_id, key_epoch, idempotency_key)
                 );",
            )
            .unwrap();
    }

    #[cfg(test)]
    fn evidence_table_counts(connection: &rusqlite::Connection) -> Vec<i64> {
        [
            "retrieval_anchors",
            "retrieval_anchor_reverse_lineage",
            "evidence_source_occurrences",
            "evidence_occurrence_sets",
            "evidence_occurrence_set_members",
            "evidence_spans",
            "evidence_span_members",
            "evidence_span_projection_receipts",
            "evidence_retriever_contributions",
            "evidence_derived_anchors",
            "evidence_assembly_receipts",
        ]
        .into_iter()
        .map(|table| {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        })
        .collect()
    }

    #[test]
    fn publish_replay_conflict_and_drilldown_are_atomic() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        let write = write_fixture("1");
        connection
            .execute(
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', ?2, 'source.fixture')",
                params![
                    write.occurrences[0].exact_source_anchor.as_str(),
                    encode(&write.owner.owner).unwrap(),
                ],
            )
            .unwrap();
        let mut executor = EvidenceAssemblyExecutor;
        for _ in 0..2 {
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            executor.execute_write(&savepoint, &write).unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
        let snapshot = connection.transaction().unwrap();
        let page = executor
            .execute_read(
                &snapshot,
                &EvidenceAssemblyReadOperationV1::ContributionPage {
                    owner: write.owner.clone(),
                    contribution_id: write.contribution.contribution_id.clone(),
                    start_ordinal: 0,
                    page_size: 1,
                },
            )
            .unwrap();
        assert!(matches!(
            page,
            EvidenceAssemblyReadResultV1::ContributionPage(Some(ref page))
                if page.occurrences.len() == 1 && page.next_ordinal.is_none()
        ));
        let mut wrong_owner = write.owner.clone();
        wrong_owner.scope_digest =
            ManifestDigest::new(format!("sha256:{}", "bb".repeat(32))).unwrap();
        assert_eq!(
            executor
                .execute_read(
                    &snapshot,
                    &EvidenceAssemblyReadOperationV1::ContributionPage {
                        owner: wrong_owner,
                        contribution_id: write.contribution.contribution_id.clone(),
                        start_ordinal: 0,
                        page_size: 1,
                    },
                )
                .unwrap(),
            EvidenceAssemblyReadResultV1::ContributionPage(None)
        );
        let mut anchor_executor = super::super::RetrievalAnchorExecutor;
        assert!(matches!(
            anchor_executor
                .execute_read(
                    &snapshot,
                    &RetrievalAnchorReadOperationV1::AnchorById {
                        anchor_id: write.contribution.anchor.anchor_id().clone(),
                        owner: write.owner.owner.clone().into(),
                    },
                )
                .unwrap(),
            RetrievalAnchorReadResultV1::Anchor(Some(StoredRetrievalAnchorRecordV1::V3(record)))
                if record == write.contribution.anchor
        ));
        snapshot.commit().unwrap();
        connection
            .execute(
                "UPDATE retrieval_anchors SET projection_generation = 'tampered'
                 WHERE anchor_id = ?1",
                [write.contribution.anchor.anchor_id().as_str()],
            )
            .unwrap();
        let snapshot = connection.transaction().unwrap();
        assert!(
            anchor_executor
                .execute_read(
                    &snapshot,
                    &RetrievalAnchorReadOperationV1::AnchorById {
                        anchor_id: write.contribution.anchor.anchor_id().clone(),
                        owner: write.owner.owner.clone().into(),
                    },
                )
                .is_err()
        );
        snapshot.commit().unwrap();

        let counts_before_conflict = evidence_table_counts(&connection);
        let conflict = write_fixture("2");
        let mut transaction = connection.transaction().unwrap();
        {
            let mut savepoint = transaction.savepoint().unwrap();
            assert!(executor.execute_write(&savepoint, &conflict).is_err());
            savepoint.rollback().unwrap();
        }
        transaction.rollback().unwrap();
        assert_eq!(
            evidence_table_counts(&connection),
            counts_before_conflict,
            "a replay conflict must not partially mutate any evidence table"
        );
    }

    #[test]
    fn canonical_identity_validation_rejects_tampered_material() {
        let write = write_fixture("1");
        let replay = write_fixture("1");
        assert_eq!(write, replay);

        let changed = write_fixture("2");
        assert_ne!(
            write.contribution.contribution_id,
            changed.contribution.contribution_id
        );
        assert_ne!(
            write.receipt.publication_receipt_id,
            changed.receipt.publication_receipt_id
        );

        let mut tampered = write;
        tampered.contribution.retriever.component_version =
            tracedecay_domain::ComponentVersion::new("2").unwrap();
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn typed_catalog_order_horizon_privacy_watermark_and_owner_tampering_is_rejected() {
        let baseline = write_fixture("1");

        let mut catalog = baseline.clone();
        catalog.span.runs[0]
            .ordering_proof
            .catalog_binding
            .catalog_digest = ManifestDigest::new(format!("sha256:{}", "ab".repeat(32))).unwrap();
        assert!(catalog.validate().is_err());

        let mut ordering = baseline.clone();
        ordering.span.runs[0].ordering_proof.source_orders[0] = 8;
        assert!(ordering.validate().is_err());

        let mut horizon = baseline.clone();
        horizon.span.horizon.knowledge_through = UtcMicros(0);
        assert!(matches!(
            horizon.span.horizon.validate_members(&horizon.occurrences),
            Err(tracedecay_store::EvidenceAssemblyStoreError::HorizonMismatch)
        ));
        assert!(horizon.validate().is_err());

        let mut privacy = baseline.clone();
        privacy.contribution.request_digest.key_epoch =
            privacy.contribution.owner.key_epoch.saturating_add(1);
        assert!(matches!(
            privacy.validate(),
            Err(tracedecay_store::EvidenceAssemblyStoreError::RequestPrivacyBindingMismatch)
        ));

        let mut watermark = baseline.clone();
        watermark.contribution.watermarks.source_watermark =
            ManifestDigest::new(format!("sha256:{}", "bc".repeat(32))).unwrap();
        assert!(watermark.validate().is_err());

        let mut owner = baseline;
        owner.occurrences[0].owner = AnchorOwnerBindingV1::for_project(
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new("project.other").unwrap(),
            PrivacyDomainId::new("privacy.fixture").unwrap(),
        )
        .unwrap();
        assert!(owner.validate().is_err());
    }

    #[test]
    fn drilldown_and_receipt_reads_reject_physical_index_tampering() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        let write = write_fixture("1");
        connection
            .execute(
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', ?2, 'source.fixture')",
                params![
                    write.occurrences[0].exact_source_anchor.as_str(),
                    encode(&write.owner.owner).unwrap(),
                ],
            )
            .unwrap();
        let mut executor = EvidenceAssemblyExecutor;
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        executor.execute_write(&savepoint, &write).unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();

        connection
            .execute(
                "UPDATE evidence_occurrence_set_members
                 SET canonical_ordinal = 7 WHERE occurrence_set_id = ?1",
                [write.occurrence_set.occurrence_set_id.as_str()],
            )
            .unwrap();
        let snapshot = connection.transaction().unwrap();
        assert!(
            executor
                .execute_read(
                    &snapshot,
                    &EvidenceAssemblyReadOperationV1::ContributionPage {
                        owner: write.owner.clone(),
                        contribution_id: write.contribution.contribution_id.clone(),
                        start_ordinal: 0,
                        page_size: 1,
                    },
                )
                .is_err()
        );
        snapshot.commit().unwrap();

        connection
            .execute(
                "UPDATE evidence_assembly_receipts
                 SET span_id = 'span.tampered' WHERE publication_receipt_id = ?1",
                [write.receipt.publication_receipt_id.as_str()],
            )
            .unwrap();
        let snapshot = connection.transaction().unwrap();
        assert!(
            executor
                .execute_read(
                    &snapshot,
                    &EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                        owner: write.owner.clone(),
                        idempotency_key: write.idempotency_key.clone(),
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn publication_rejects_cross_project_source_anchor_without_partial_rows() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        connection
            .execute(
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES ('retrieval.source.fixture', '{}', ?1, 'source.fixture')",
                [encode(
                    &AnchorOwnerBindingV1::for_project(
                        UserProfileId::new("profile.fixture").unwrap(),
                        ProjectId::new("project.other").unwrap(),
                        PrivacyDomainId::new("privacy.fixture").unwrap(),
                    )
                    .unwrap(),
                )
                .unwrap()],
            )
            .unwrap();
        let write = write_fixture("1");
        let mut transaction = connection.transaction().unwrap();
        {
            let mut savepoint = transaction.savepoint().unwrap();
            assert!(
                EvidenceAssemblyExecutor
                    .execute_write(&savepoint, &write)
                    .is_err()
            );
            savepoint.rollback().unwrap();
        }
        transaction.rollback().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM evidence_assembly_receipts",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM evidence_source_occurrences",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn publication_rejects_unresolved_v2_owner_without_partial_rows() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        connection
            .execute(
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES ('retrieval.source.fixture', '{}', ?1, 'source.fixture')",
                [encode(&tracedecay_domain::FactOwnerV1::Project {
                    project_id: ProjectId::new("project.fixture").unwrap(),
                })
                .unwrap()],
            )
            .unwrap();
        let write = write_fixture("1");
        let mut transaction = connection.transaction().unwrap();
        {
            let mut savepoint = transaction.savepoint().unwrap();
            assert!(
                EvidenceAssemblyExecutor
                    .execute_write(&savepoint, &write)
                    .is_err()
            );
            savepoint.rollback().unwrap();
        }
        transaction.rollback().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM evidence_assembly_receipts",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM evidence_source_occurrences",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }
}
