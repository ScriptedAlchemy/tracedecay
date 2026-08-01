//! Retrieval-anchor liveness as evidence assembly needs to see it.
//!
//! The disposition tables these read are appended to by the root authority
//! in `crates/tracedecay-runtime-core/src/db/retrieval_anchor_authority.rs`
//! as well, so this module only ever reads them.

use rusqlite::{OptionalExtension, params};
use tracedecay_domain::RetrievalAnchorRecordV3;
use tracedecay_store::{EvidenceSourceOccurrenceRecordV1, RetrievalAnchorOwnerV1};

use super::super::support::{decode, encode, invalid};

pub(super) fn evidence_anchor_is_current(
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

pub(super) fn require_source_anchor_current(
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
