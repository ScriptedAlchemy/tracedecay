//! Writing, advancing, and reading one observation.
//!
//! The executor owns the transaction shape; the siblings own the pieces it
//! composes — [`authority`] the anchor/provenance/receipt rows a write persists
//! and a replay verifies, and [`rows`] the single projection every read decodes
//! through.

use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationCollisionOutcomeV1, ProjectionGenerationId,
    classify_observation_collision,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCoverageReason, ObservationCursorAdvance,
    ObservationReadOperationV1, ObservationReadResultV1, ProjectionRebuildProgressV1,
    ProjectionRebuildStateV1, SESSION_MESSAGE_PROJECTOR_VERSION,
};

use super::support::{decode, encode, invalid};

mod authority;
mod rows;

use authority::{
    cursor_advance_receipt_matches, persist_repository_provenance, persist_retrieval_anchor,
    persist_sanitization_receipt, read_cursor, verify_observation_authority,
};
use rows::{
    OBSERVATION_ROW_PROJECTION, decode_nonnegative, decode_observation_row, encoded_observation_row,
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
        let observation_json = encode(observation)?;
        let committed_cursor_json = encode(write.next_cursor())?;
        let receipt = observation.receipt();
        let receipt_json = encode(receipt)?;
        let receipt_id = receipt.receipt().receipt_id().as_str();
        let payload_digest = observation.payload_reference().digest().as_str();
        let existing = savepoint
            .query_row(
                "SELECT payload_digest, receipt_id, observation_json
                 FROM observations WHERE observation_id = ?1",
                [observation.observation_id().as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_digest, stored_receipt_id, stored_observation)) = existing {
            let stored_observation = decode(stored_observation)?;
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
                || stored_digest != payload_digest
                || stored_receipt_id != receipt_id
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

        savepoint.execute(
            "INSERT INTO observations (
                observation_id, payload_digest, receipt_id,
                observation_json, committed_cursor_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                observation.observation_id().as_str(),
                payload_digest,
                receipt_id,
                observation_json,
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
            ObservationReadOperationV1::NextQueuedProjection { now_micros } => {
                let observation_id = snapshot
                    .query_row(
                        "SELECT observation_id FROM projection_queue
                         WHERE next_retry_at_micros <= ?2
                           AND observation_sequence = (
                             SELECT MIN(observation_sequence) FROM projection_queue
                           )
                           AND NOT EXISTS (
                           SELECT 1 FROM observation_projection_rebuilds
                           WHERE projector_version = ?1
                         )
                         LIMIT 1",
                        (SESSION_MESSAGE_PROJECTOR_VERSION, now_micros),
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

#[cfg(test)]
mod tests;
