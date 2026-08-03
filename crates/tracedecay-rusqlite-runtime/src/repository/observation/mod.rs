//! Writing, advancing, and reading one observation.
//!
//! The executor owns the transaction shape; the siblings own the pieces it
//! composes — [`authority`] the anchor/provenance/receipt rows a write persists
//! and a replay verifies, and [`rows`] the single projection every read decodes
//! through.

use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{
    CanonicalObservationIdV1, ContentDigest, DurableObservationV1, ObservationCollisionOutcomeV1,
    ProjectionGenerationId, canonical_json_bytes, classify_observation_collision,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCoverageReason, ObservationCursorAdvance,
    ObservationReadOperationV1, ObservationReadResultV1, ProjectionRebuildProgressV1,
    ProjectionRebuildStateV1, SESSION_MESSAGE_PROJECTOR_VERSION,
};

use super::support::{encode, invalid};

mod authority;
mod rows;

use authority::{
    cursor_advance_receipt_matches, persist_repository_provenance, persist_retrieval_anchor,
    persist_sanitization_receipt, read_cursor, verify_observation_authority,
};
use rows::{
    OBSERVATION_ROW_PROJECTION, decode_nonnegative, decode_observation_row,
    encoded_observation_row, read_stored_observation_content,
};

#[derive(Clone, Default)]
pub struct ObservationExecutor;

impl ObservationExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        write: &AnchoredObservationWrite,
    ) -> rusqlite::Result<()> {
        let observation = write.observation();
        let source_json = encode(observation.source())?;
        let scope_json = encode(observation.scope())?;
        let committed_cursor_json = encode(write.next_cursor())?;
        let receipt = observation.receipt();
        let receipt_json = encode(receipt)?;
        let receipt_id = receipt.receipt().receipt_id().as_str();
        let payload_digest = observation.payload_reference().digest().as_str();
        let existing =
            read_stored_observation_content(savepoint, observation.observation_id().as_str())?;
        if let Some(existing) = existing {
            let stored_observation = existing.observation;
            let collision = classify_observation_collision(&stored_observation, observation);
            if collision == ObservationCollisionOutcomeV1::ExactDuplicate
                && stored_observation.identity() != observation.identity()
            {
                let identity = observation.identity();
                let mut advance = ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
                    identity.source().clone(),
                    identity.scope().clone(),
                    identity.generation(),
                    identity.ordering_domain(),
                    write.expected_cursor().cloned(),
                    identity.position(),
                    ObservationCoverageReason::DuplicateObservation,
                    observation.receipt().clone(),
                )
                .map_err(invalid)?;
                match (
                    write.next_cursor().file_identity(),
                    write.next_cursor().resume_fingerprint(),
                ) {
                    (Some(file_identity), Some(resume_fingerprint)) => {
                        advance = advance.with_resume_checkpoint(file_identity, resume_fingerprint);
                    }
                    (None, None) => {}
                    _ => return Err(invalid("cursor resume checkpoint is incomplete")),
                }
                return self.execute_cursor_advance(savepoint, &advance);
            }
            if collision != ObservationCollisionOutcomeV1::ExactDuplicate
                || existing.payload_digest != payload_digest
                || existing.receipt_id != receipt_id
                || stored_observation != *observation
            {
                return Err(invalid("observation identity collision"));
            }
            let stored_receipt: String = savepoint.query_row(
                "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
                [receipt_id],
                |row| row.get(0),
            )?;
            if stored_receipt != receipt_json {
                return Err(invalid("sanitization receipt identity collision"));
            }
            verify_observation_authority(savepoint, write)?;
            return Ok(());
        }

        let actual_cursor = read_cursor(savepoint, &source_json, &scope_json)?;
        if actual_cursor.as_ref() != write.expected_cursor() {
            return Err(invalid("observation source cursor conflict"));
        }

        persist_sanitization_receipt(savepoint, receipt)?;
        let content_digest = persist_observation_content(savepoint, observation, receipt_id)?;

        savepoint.execute(
            "INSERT INTO observations (
                observation_id, payload_digest, receipt_id,
                content_digest, committed_cursor_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                observation.observation_id().as_str(),
                payload_digest,
                receipt_id,
                content_digest.as_str(),
                committed_cursor_json,
            ],
        )?;
        let sequence = savepoint.last_insert_rowid();
        persist_retrieval_anchor(savepoint, write.retrieval_anchor())?;
        savepoint.execute(
            "INSERT INTO observation_retrieval_anchors (observation_id, anchor_id)
             VALUES (?1, ?2)",
            params![
                observation.observation_id().as_str(),
                write.retrieval_anchor_id().as_str(),
            ],
        )?;
        persist_repository_provenance(
            savepoint,
            observation.observation_id().as_str(),
            write.repository_provenance_attachment(),
        )?;
        savepoint.execute(
            "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_json, scope_json) DO UPDATE SET
                cursor_json = excluded.cursor_json",
            params![source_json, scope_json, committed_cursor_json],
        )?;
        savepoint.execute(
            "INSERT INTO projection_queue (observation_id, observation_sequence)
             VALUES (?1, ?2)",
            params![observation.observation_id().as_str(), sequence],
        )?;
        Ok(())
    }

    pub fn execute_cursor_advance(
        &mut self,
        savepoint: &Savepoint<'_>,
        advance: &ObservationCursorAdvance,
    ) -> rusqlite::Result<()> {
        let source_json = encode(advance.next_cursor().source())?;
        let scope_json = encode(advance.next_cursor().scope())?;
        let actual_cursor = read_cursor(savepoint, &source_json, &scope_json)?;
        if actual_cursor.as_ref() == Some(advance.next_cursor()) {
            if cursor_advance_receipt_matches(savepoint, &source_json, &scope_json, advance)? {
                return Ok(());
            }
            return Err(invalid("source cursor advance identity collision"));
        }
        if actual_cursor.as_ref() != advance.expected_cursor() {
            return Err(invalid("observation source cursor conflict"));
        }
        if let Some(receipt) = advance.sanitization_receipt() {
            persist_sanitization_receipt(savepoint, receipt)?;
        }
        let coverage_json = encode(&advance.coverage())?;
        savepoint.execute(
            "INSERT INTO source_cursor_advances (
                source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source_json, scope_json, coverage_json) DO NOTHING",
            params![
                source_json,
                scope_json,
                coverage_json,
                advance.reason().as_str(),
                advance
                    .sanitization_receipt()
                    .map(|receipt| receipt.receipt().receipt_id().as_str()),
            ],
        )?;
        if !cursor_advance_receipt_matches(savepoint, &source_json, &scope_json, advance)? {
            return Err(invalid("source cursor advance identity collision"));
        }
        savepoint.execute(
            "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_json, scope_json) DO UPDATE SET
                cursor_json = excluded.cursor_json",
            params![source_json, scope_json, encode(advance.next_cursor())?],
        )?;
        Ok(())
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &ObservationReadOperationV1,
    ) -> rusqlite::Result<ObservationReadResultV1> {
        match operation {
            ObservationReadOperationV1::SourceCursor { source, scope } => {
                let cursor = read_cursor(snapshot, &encode(source)?, &encode(scope)?)?;
                Ok(ObservationReadResultV1::SourceCursor(cursor))
            }
            ObservationReadOperationV1::Observation { observation_id } => {
                let row = snapshot
                    .query_row(
                        &format!(
                            "{OBSERVATION_ROW_PROJECTION}
                             WHERE observation.observation_id = ?1"
                        ),
                        [observation_id.as_str()],
                        encoded_observation_row,
                    )
                    .optional()?;
                let value = row.map(decode_observation_row).transpose()?;
                if value
                    .as_ref()
                    .is_some_and(|row| row.observation.observation_id() != observation_id)
                {
                    return Err(invalid("observation row identity mismatch"));
                }
                Ok(ObservationReadResultV1::Observation(Box::new(value)))
            }
            ObservationReadOperationV1::RetrievalAnchorByAlias { scope, alias } => {
                let anchor_id = snapshot
                    .query_row(
                        "SELECT anchor_id FROM retrieval_anchor_aliases
                         WHERE owner_json = ?1 AND alias_kind = ?2 AND locator_digest = ?3",
                        params![
                            encode(scope)?,
                            encode(&alias.kind())?,
                            encode(alias.locator_digest())?,
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .map(tracedecay_domain::RetrievalAnchorId::new)
                    .transpose()
                    .map_err(invalid)?;
                Ok(ObservationReadResultV1::RetrievalAnchorByAlias(anchor_id))
            }
            ObservationReadOperationV1::Replay {
                after_sequence,
                limit,
            } => {
                if *limit == 0 || *limit > 1_000 {
                    return Err(invalid(
                        "observation replay limit must be between 1 and 1000",
                    ));
                }
                let after_sequence = i64::try_from(*after_sequence)
                    .map_err(|_| invalid("observation replay frontier exceeds SQLite integer"))?;
                let mut statement = snapshot.prepare(&format!(
                    "{OBSERVATION_ROW_PROJECTION}
                     WHERE observation.sequence > ?1
                     ORDER BY observation.sequence ASC LIMIT ?2"
                ))?;
                let rows = statement.query_map(
                    params![after_sequence, i64::from(*limit)],
                    encoded_observation_row,
                )?;
                let mut observations = Vec::new();
                for row in rows {
                    observations.push(decode_observation_row(row?)?);
                }
                Ok(ObservationReadResultV1::Replay(observations))
            }
            ObservationReadOperationV1::NextQueuedProjection => {
                let observation_id = snapshot
                    .query_row(
                        "SELECT observation_id FROM projection_queue
                         WHERE NOT EXISTS (
                           SELECT 1 FROM observation_projection_rebuilds
                           WHERE projector_version = ?1
                         )
                         ORDER BY observation_sequence ASC LIMIT 1",
                        [SESSION_MESSAGE_PROJECTOR_VERSION],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .map(CanonicalObservationIdV1::new)
                    .transpose()
                    .map_err(invalid)?;
                Ok(ObservationReadResultV1::NextQueuedProjection(
                    observation_id,
                ))
            }
            ObservationReadOperationV1::ProjectionCheckpoint => {
                let checkpoint = snapshot
                    .query_row(
                        "SELECT last_sequence FROM observation_projection_checkpoints
                         WHERE projector_version = ?1",
                        [SESSION_MESSAGE_PROJECTOR_VERSION],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .map(|sequence| {
                        u64::try_from(sequence)
                            .map_err(|_| invalid("negative projection checkpoint"))
                    })
                    .transpose()?
                    .unwrap_or(0);
                Ok(ObservationReadResultV1::ProjectionCheckpoint(checkpoint))
            }
            ObservationReadOperationV1::ProjectionRebuildProgress => {
                let progress = snapshot
                    .query_row(
                        "SELECT generation, frontier_sequence, aliases_staged_through, staged_through,
                                projected_rows, skipped_observations, state
                         FROM observation_projection_rebuilds WHERE projector_version = ?1",
                        [SESSION_MESSAGE_PROJECTOR_VERSION],
                        |row| {
                            let state = match row.get::<_, String>(6)?.as_str() {
                                "aliasing" => ProjectionRebuildStateV1::Aliasing,
                                "building" => ProjectionRebuildStateV1::Building,
                                "ready" => ProjectionRebuildStateV1::Ready,
                                _ => return Err(invalid("unknown projection rebuild state")),
                            };
                            Ok(ProjectionRebuildProgressV1 {
                                generation: ProjectionGenerationId::new(
                                    row.get::<_, String>(0)?,
                                )
                                .map_err(invalid)?,
                                frontier_sequence: decode_nonnegative(
                                    row.get(1)?,
                                    "negative projection rebuild frontier",
                                )?,
                                aliases_staged_through: decode_nonnegative(
                                    row.get(2)?,
                                    "negative projection rebuild alias frontier",
                                )?,
                                staged_through: decode_nonnegative(
                                    row.get(3)?,
                                    "negative projection rebuild staged frontier",
                                )?,
                                projected_rows: decode_nonnegative(
                                    row.get(4)?,
                                    "negative projection rebuild row count",
                                )?,
                                skipped_observations: decode_nonnegative(
                                    row.get(5)?,
                                    "negative projection rebuild skip count",
                                )?,
                                state,
                            })
                        },
                    )
                    .optional()?;
                Ok(ObservationReadResultV1::ProjectionRebuildProgress(progress))
            }
        }
    }
}

fn persist_observation_content(
    savepoint: &Savepoint<'_>,
    observation: &DurableObservationV1,
    receipt_id: &str,
) -> rusqlite::Result<ContentDigest> {
    let bytes = canonical_json_bytes(observation).map_err(invalid)?;
    let content_digest = ContentDigest::of_bytes(&bytes);
    let byte_count =
        i64::try_from(bytes.len()).map_err(|_| invalid("observation content is too large"))?;
    let text = std::str::from_utf8(&bytes).map_err(invalid)?;
    let char_count = i64::try_from(text.chars().count())
        .map_err(|_| invalid("observation content character count is too large"))?;
    savepoint.execute(
        "INSERT INTO session_content_objects (
            content_digest, inline_bytes, durable_file_locator, byte_count, char_count
         ) VALUES (?1, ?2, NULL, ?3, ?4)
         ON CONFLICT(content_digest) DO NOTHING",
        params![
            content_digest.as_str(),
            bytes.as_slice(),
            byte_count,
            char_count
        ],
    )?;
    let stored = savepoint.query_row(
        "SELECT inline_bytes, durable_file_locator, byte_count, char_count
         FROM session_content_objects
         WHERE content_digest = ?1",
        [content_digest.as_str()],
        |row| {
            Ok((
                row.get::<_, Option<Vec<u8>>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )?;
    if stored != (Some(bytes), None, byte_count, char_count) {
        return Err(invalid("session content object identity collision"));
    }

    savepoint.execute(
        "INSERT INTO session_content_references (
            owner_kind, owner_id, content_kind, content_digest,
            sanitization_receipt_id, retrieval_anchor_id
         ) VALUES ('projection', ?1, 'observation_json', ?2, ?3, NULL)
         ON CONFLICT(owner_kind, owner_id) DO NOTHING",
        params![
            observation.observation_id().as_str(),
            content_digest.as_str(),
            receipt_id
        ],
    )?;
    let reference = savepoint.query_row(
        "SELECT content_kind, content_digest, sanitization_receipt_id, retrieval_anchor_id
         FROM session_content_references
         WHERE owner_kind = 'projection' AND owner_id = ?1",
        [observation.observation_id().as_str()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        },
    )?;
    if reference
        != (
            "observation_json".to_owned(),
            content_digest.as_str().to_owned(),
            Some(receipt_id.to_owned()),
            None,
        )
    {
        return Err(invalid("session content projection reference collision"));
    }
    Ok(content_digest)
}

#[cfg(test)]
mod tests;
