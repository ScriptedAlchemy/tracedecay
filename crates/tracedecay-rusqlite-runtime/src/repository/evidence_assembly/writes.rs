//! The evidence assembly write path's table-by-table inserts.
//!
//! Every one of these is a replay-safe write: see
//! [`idempotent_insert`](super::super::support::idempotent_insert) for the
//! contract they all share.

use tracedecay_domain::{RetrievalAnchorRecordV3, RetrievalAnchorTargetV3};
use tracedecay_store::EvidenceAssemblyWriteV1;

use super::super::support::{encode, idempotent_insert, invalid, usize_to_i64};

pub(super) fn insert_anchor(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV3,
) -> rusqlite::Result<()> {
    anchor.validate().map_err(invalid)?;
    idempotent_insert(
        connection,
        "retrieval_anchors",
        &[("anchor_id", anchor.anchor_id().as_str().into())],
        &[
            ("anchor_json", encode(anchor)?.into()),
            ("owner_json", encode(anchor.owner())?.into()),
            (
                "projection_generation",
                anchor.projection_generation().as_str().into(),
            ),
        ],
        "retrieval anchor replay conflict",
    )
}

/// Writes one row of an immutable record table, which is any table keyed by a
/// single id and carrying the canonical `record_digest`/`record_json` pair plus
/// whatever columns it denormalizes out of that record for indexing.
pub(super) fn insert_immutable(
    connection: &rusqlite::Connection,
    table: &'static str,
    id_column: &'static str,
    id: &str,
    record_digest: String,
    record_json: String,
    extra: &[(&'static str, String)],
) -> rusqlite::Result<()> {
    let mut values = vec![
        ("record_digest", record_digest.into()),
        ("record_json", record_json.into()),
    ];
    values.extend(
        extra
            .iter()
            .map(|(column, value)| (*column, value.clone().into())),
    );
    idempotent_insert(
        connection,
        table,
        &[(id_column, id.into())],
        &values,
        &format!("{table} immutable replay conflict"),
    )
}

pub(super) fn insert_membership(
    connection: &rusqlite::Connection,
    table: &'static str,
    parent_column: &'static str,
    parent_id: &str,
    ordinal_column: &'static str,
    ordinal: usize,
    occurrence_id: &str,
) -> rusqlite::Result<()> {
    idempotent_insert(
        connection,
        table,
        &[
            (parent_column, parent_id.into()),
            (
                ordinal_column,
                usize_to_i64(ordinal, "evidence membership ordinal")?.into(),
            ),
        ],
        &[("occurrence_id", occurrence_id.into())],
        &format!("{table} immutable replay conflict"),
    )
}

pub(super) fn insert_span_membership(
    connection: &rusqlite::Connection,
    span_id: &str,
    assembly_ordinal: usize,
    run_ordinal: usize,
    run_member_ordinal: usize,
    occurrence_id: &str,
) -> rusqlite::Result<()> {
    idempotent_insert(
        connection,
        "evidence_span_members",
        &[
            ("span_id", span_id.into()),
            (
                "assembly_ordinal",
                usize_to_i64(assembly_ordinal, "evidence assembly ordinal")?.into(),
            ),
        ],
        &[
            (
                "run_ordinal",
                usize_to_i64(run_ordinal, "evidence run ordinal")?.into(),
            ),
            (
                "run_member_ordinal",
                usize_to_i64(run_member_ordinal, "evidence run member ordinal")?.into(),
            ),
            ("occurrence_id", occurrence_id.into()),
        ],
        "evidence span membership replay conflict",
    )
}

/// Records reverse lineage for every occurrence's source anchor.
///
/// `source_owner_jsons` carries the `owner_json` each source anchor was already
/// read under in `execute_write` (via `require_source_anchor_current`), parallel
/// to `write.occurrences`, so this pass reuses those values instead of reading
/// each `retrieval_anchors` row a second time.
pub(super) fn publish_reverse_lineage(
    connection: &rusqlite::Connection,
    write: &EvidenceAssemblyWriteV1,
    source_owner_jsons: &[String],
) -> rusqlite::Result<()> {
    for (occurrence, owner_json) in write.occurrences.iter().zip(source_owner_jsons) {
        for (kind, derivative_id) in [
            ("span", write.span.span_id.as_str()),
            ("contribution", write.contribution.contribution_id.as_str()),
        ] {
            idempotent_insert(
                connection,
                "retrieval_anchor_reverse_lineage",
                &[
                    (
                        "source_anchor_id",
                        occurrence.exact_source_anchor.as_str().into(),
                    ),
                    ("owner_json", owner_json.clone().into()),
                    ("derivative_kind", kind.into()),
                    ("derivative_id", derivative_id.into()),
                ],
                &[("direct_evidence", 1_i64.into())],
                "evidence reverse lineage replay conflict",
            )?;
        }
    }
    Ok(())
}

pub(super) fn insert_derived_anchor(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorRecordV3,
    owner_digest: &str,
) -> rusqlite::Result<()> {
    let (target_kind, target_id) = evidence_target(anchor)?;
    idempotent_insert(
        connection,
        "evidence_derived_anchors",
        &[("anchor_id", anchor.anchor_id().as_str().into())],
        &[
            ("owner_digest", owner_digest.into()),
            ("target_kind", target_kind.into()),
            ("target_id", target_id.into()),
            ("anchor_json", encode(anchor)?.into()),
        ],
        "evidence derived anchor replay conflict",
    )
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
