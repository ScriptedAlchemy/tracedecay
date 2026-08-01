//! Retrieval-anchor liveness as evidence assembly needs to see it.
//!
//! The disposition tables these read are appended to by the root authority
//! in `crates/tracedecay-runtime-core/src/db/retrieval_anchor_authority.rs`
//! as well, so this module only ever reads them.

use std::collections::{BTreeSet, HashMap};

use rusqlite::{OptionalExtension, params, params_from_iter};
use tracedecay_domain::RetrievalAnchorRecordV3;
use tracedecay_store::{EvidenceSourceOccurrenceRecordV1, RetrievalAnchorOwnerV1};

use super::super::support::{decode, encode, invalid};

/// The largest `anchor_id IN (...)` batch a single prepared statement binds.
///
/// A drilldown page carries at most 256 occurrences, each contributing an
/// occurrence anchor and a source anchor, so the deduplicated set never
/// approaches SQLite's default variable ceiling — but chunking keeps the
/// batched liveness load correct if a caller ever exceeds it.
const ANCHOR_LIVENESS_BATCH: usize = 500;

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

/// Confirms the exact source anchor an occurrence names is present and active,
/// returning the anchor's stored `owner_json` so a caller in the same
/// transaction can reuse it instead of reading the row a second time.
pub(super) fn require_source_anchor_current(
    connection: &rusqlite::Connection,
    occurrence: &EvidenceSourceOccurrenceRecordV1,
) -> rusqlite::Result<String> {
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
        Ok(owner_json)
    } else {
        Err(invalid("evidence source anchor is disposed"))
    }
}

/// One `retrieval_anchors` row as the liveness checks need to see it.
struct AnchorRow {
    owner_json: String,
    anchor_json: String,
    projection_generation: String,
}

/// A batch-loaded view of anchor rows and their latest dispositions, so a page
/// of occurrences can be checked for liveness without a per-occurrence pair of
/// round trips.
///
/// The cached checks reproduce [`evidence_anchor_is_current`] and
/// [`require_source_anchor_current`] exactly, reading the same columns and
/// returning the same `Ok`/`Err` outcomes — they only replace the individual
/// `SELECT`s with two `anchor_id IN (...)` loads made up front.
pub(super) struct AnchorLivenessCache {
    anchors: HashMap<String, AnchorRow>,
    /// `(anchor_id, owner_json)` to the newest disposition state recorded for
    /// it, mirroring [`latest_disposition_state`]'s `ORDER BY sequence DESC`.
    dispositions: HashMap<(String, String), String>,
}

/// Loads every anchor row and latest disposition for `anchor_ids` in two
/// batched statements, regardless of how many occurrences reference them.
pub(super) fn load_anchor_liveness(
    connection: &rusqlite::Connection,
    anchor_ids: &BTreeSet<String>,
) -> rusqlite::Result<AnchorLivenessCache> {
    let mut anchors: HashMap<String, AnchorRow> = HashMap::new();
    let mut latest: HashMap<(String, String), (i64, String)> = HashMap::new();
    let ids: Vec<&str> = anchor_ids.iter().map(String::as_str).collect();
    for chunk in ids.chunks(ANCHOR_LIVENESS_BATCH) {
        let placeholders = (1..=chunk.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");

        let mut anchor_statement = connection.prepare(&format!(
            "SELECT anchor_id, owner_json, anchor_json, projection_generation
             FROM retrieval_anchors
             WHERE anchor_id IN ({placeholders})",
        ))?;
        let anchor_rows =
            anchor_statement.query_map(params_from_iter(chunk.iter().copied()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    AnchorRow {
                        owner_json: row.get::<_, String>(1)?,
                        anchor_json: row.get::<_, String>(2)?,
                        projection_generation: row.get::<_, String>(3)?,
                    },
                ))
            })?;
        for row in anchor_rows {
            let (anchor_id, anchor) = row?;
            anchors.insert(anchor_id, anchor);
        }

        let mut disposition_statement = connection.prepare(&format!(
            "SELECT anchor_id, owner_json, state, sequence
             FROM retrieval_anchor_dispositions
             WHERE anchor_id IN ({placeholders})",
        ))?;
        let disposition_rows =
            disposition_statement.query_map(params_from_iter(chunk.iter().copied()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
        for row in disposition_rows {
            let (anchor_id, owner_json, state, sequence) = row?;
            latest
                .entry((anchor_id, owner_json))
                .and_modify(|current| {
                    if sequence >= current.0 {
                        *current = (sequence, state.clone());
                    }
                })
                .or_insert((sequence, state));
        }
    }
    let dispositions = latest
        .into_iter()
        .map(|(key, (_, state))| (key, state))
        .collect();
    Ok(AnchorLivenessCache {
        anchors,
        dispositions,
    })
}

impl AnchorLivenessCache {
    /// The batched equivalent of the free [`evidence_anchor_is_current`].
    pub(super) fn evidence_anchor_is_current(
        &self,
        anchor: &RetrievalAnchorRecordV3,
    ) -> rusqlite::Result<bool> {
        let owner_json = encode(anchor.owner())?;
        // A missing row, or one filed under a different owner, is exactly the
        // `WHERE anchor_id = ?1 AND owner_json = ?2` miss the row query returns.
        let Some(row) = self
            .anchors
            .get(anchor.anchor_id().as_str())
            .filter(|row| row.owner_json == owner_json)
        else {
            return Ok(false);
        };
        if row.anchor_json != encode(anchor)?
            || row.projection_generation != anchor.projection_generation().as_str()
        {
            return Err(invalid("evidence retrieval anchor persistence mismatch"));
        }
        let state = self
            .dispositions
            .get(&(anchor.anchor_id().as_str().to_owned(), owner_json));
        Ok(state
            .map(String::as_str)
            .is_none_or(|state| state == "active"))
    }

    /// The batched equivalent of the free [`require_source_anchor_current`].
    pub(super) fn require_source_anchor_current(
        &self,
        occurrence: &EvidenceSourceOccurrenceRecordV1,
    ) -> rusqlite::Result<()> {
        let row = self
            .anchors
            .get(occurrence.exact_source_anchor.as_str())
            .ok_or_else(|| invalid("evidence source anchor unavailable"))?;
        let source_owner: RetrievalAnchorOwnerV1 = decode(row.owner_json.clone())?;
        if !source_owner_matches_assembly(&source_owner, &occurrence.owner) {
            return Err(invalid("evidence source anchor owner mismatch"));
        }
        let state = self.dispositions.get(&(
            occurrence.exact_source_anchor.as_str().to_owned(),
            row.owner_json.clone(),
        ));
        if state
            .map(String::as_str)
            .is_none_or(|state| state == "active")
        {
            Ok(())
        } else {
            Err(invalid("evidence source anchor is disposed"))
        }
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
