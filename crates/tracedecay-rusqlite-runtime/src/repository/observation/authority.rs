//! The authority rows an observation write persists and replays against.
//!
//! Each `persist_*` here is paired with the `verify_*` that a replay runs
//! instead, so a re-applied write reads back exactly the rows the first apply
//! wrote or fails as a collision.

use rusqlite::{OptionalExtension, params};
use tracedecay_domain::{ObservationSourceCursorV1, RetrievalAnchorRecordV2};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCursorAdvance, RepositoryProvenanceAttachmentV1,
};

use super::super::support::{decode, encode, invalid};

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
            "SELECT reason, receipt_id FROM source_cursor_advances
             WHERE source_json = ?1 AND scope_json = ?2 AND coverage_json = ?3",
            params![source_json, scope_json, encode(&advance.coverage())?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let expected_receipt_id = advance
        .sanitization_receipt()
        .map(|receipt| receipt.receipt().receipt_id().as_str());
    if stored.as_ref().is_none_or(|(reason, receipt_id)| {
        reason != advance.reason().as_str() || receipt_id.as_deref() != expected_receipt_id
    }) {
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
    if stored.as_ref()
        != Some(&(
            encode(anchor)?,
            owner_json.clone(),
            anchor.projection_generation().as_str().to_owned(),
        ))
    {
        return Err(invalid("retrieval anchor identity collision"));
    }
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
        if stored_anchor_id.as_deref() != Some(anchor.anchor_id().as_str()) {
            return Err(invalid("retrieval anchor alias collision"));
        }
    }
    let alias_count = connection.query_row(
        "SELECT COUNT(*) FROM retrieval_anchor_aliases
         WHERE owner_json = ?1 AND anchor_id = ?2",
        params![owner_json, anchor.anchor_id().as_str()],
        |row| row.get::<_, i64>(0),
    )?;
    if usize::try_from(alias_count).ok() != Some(anchor.aliases().len()) {
        return Err(invalid("retrieval anchor alias collision"));
    }
    Ok(())
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
        return Err(invalid("observation repository provenance collision"));
    }
    if let Some(anchor) = attachment.anchor() {
        verify_retrieval_anchor(connection, anchor)?;
    }
    Ok(())
}

pub(super) fn read_cursor(
    connection: &rusqlite::Connection,
    source_json: &str,
    scope_json: &str,
) -> rusqlite::Result<Option<ObservationSourceCursorV1>> {
    connection
        .query_row(
            "SELECT cursor_json FROM source_cursors
             WHERE source_json = ?1 AND scope_json = ?2",
            params![source_json, scope_json],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(decode)
        .transpose()
}
