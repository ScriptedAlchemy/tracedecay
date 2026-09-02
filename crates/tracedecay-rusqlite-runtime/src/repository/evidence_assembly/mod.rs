//! Publishing and reading one evidence assembly.
//!
//! The executor owns the transaction shape; the siblings own the pieces it
//! composes — [`writes`] the replay-safe table inserts, [`reads`] the two read
//! operations, and [`anchor_state`] the retrieval-anchor liveness both consult.

use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_store::{
    EvidenceAssemblyReadOperationV1, EvidenceAssemblyReadResultV1, EvidenceAssemblyWriteV1,
};

use super::support::{canonical_digest, decode, encode, invalid, u64_to_i64};

mod anchor_state;
mod reads;
mod writes;

use anchor_state::require_source_anchor_current;
use writes::{
    insert_anchor, insert_derived_anchor, insert_immutable, insert_membership,
    insert_span_membership, publish_reverse_lineage,
};

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

        let mut source_owner_jsons = Vec::with_capacity(write.occurrences.len());
        for occurrence in &write.occurrences {
            let source_owner_json = require_source_anchor_current(savepoint, occurrence)?;
            source_owner_jsons.push(source_owner_json);
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

        publish_reverse_lineage(savepoint, write, &source_owner_jsons)?;
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
            } => reads::publication_by_idempotency(snapshot, owner, idempotency_key),
            EvidenceAssemblyReadOperationV1::ContributionPage {
                owner,
                contribution_id,
                start_ordinal,
                page_size,
            } => reads::contribution_page(
                snapshot,
                owner,
                contribution_id,
                *start_ordinal,
                *page_size,
            ),
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
pub mod tests;
