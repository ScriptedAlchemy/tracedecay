use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{
    CanonicalObservationIdV1, EvidenceAvailabilityV1, GenerationBoundRepositoryProvenanceV1,
    ObservationCollisionOutcomeV1, ObservationSourceCursorV1, ProjectionGenerationId,
    RetrievalAnchorRecordV2, classify_observation_collision,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCommitReceipt, ObservationCoverageReason,
    ObservationCursorAdvance, ObservationReadOperationV1, ObservationReadResultV1,
    ProjectionRebuildProgressV1, ProjectionRebuildStateV1, RepositoryProvenanceAttachmentV1,
    SESSION_MESSAGE_PROJECTOR_VERSION, StoredObservationRowV1,
};

use super::support::{decode, encode, invalid};

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

fn decode_nonnegative(value: i64, message: &'static str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| invalid(message))
}

type EncodedObservationRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

/// The single projection every observation read decodes through.
///
/// The outer joins are all optional by schema, so the missing halves are
/// rejected by [`decode_observation_row`] rather than by the query. Callers
/// append their own `WHERE`/`ORDER BY`/`LIMIT` clauses; the column list and its
/// order are fixed here because [`encoded_observation_row`] reads them
/// positionally.
const OBSERVATION_ROW_PROJECTION: &str =
    "SELECT observation.sequence, observation.observation_json,
            observation.committed_cursor_json, anchor.anchor_json,
            anchor.projection_generation, repository.availability_json,
            repository.capture_json, repository_anchor.anchor_json,
            repository.owner_json,
            EXISTS(
                SELECT 1 FROM projection_queue
                WHERE projection_queue.observation_id =
                      observation.observation_id
            )
     FROM observations AS observation
     LEFT JOIN observation_retrieval_anchors AS binding
       ON binding.observation_id = observation.observation_id
     LEFT JOIN retrieval_anchors AS anchor
       ON anchor.anchor_id = binding.anchor_id
     LEFT JOIN observation_repository_provenance AS repository
       ON repository.observation_id = observation.observation_id
     LEFT JOIN retrieval_anchors AS repository_anchor
       ON repository_anchor.anchor_id = repository.retrieval_anchor_id";

/// Reads one [`OBSERVATION_ROW_PROJECTION`] row in column order.
fn encoded_observation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EncodedObservationRow> {
    Ok((
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, Option<String>>(3)?,
        row.get::<_, Option<String>>(4)?,
        row.get::<_, Option<String>>(5)?,
        row.get::<_, Option<String>>(6)?,
        row.get::<_, Option<String>>(7)?,
        row.get::<_, Option<String>>(8)?,
        row.get::<_, i64>(9)?,
    ))
}

fn decode_observation_row(
    (
        sequence,
        observation,
        cursor,
        retrieval_anchor,
        projection_generation,
        repository_availability,
        repository_capture,
        repository_anchor,
        repository_owner,
        projection_queued,
    ): EncodedObservationRow,
) -> rusqlite::Result<StoredObservationRowV1> {
    let repository_availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> =
        decode(
            repository_availability
                .ok_or_else(|| invalid("observation repository provenance is missing"))?,
        )?;
    let repository_capture = repository_capture
        .map(decode::<GenerationBoundRepositoryProvenanceV1>)
        .transpose()?;
    if repository_availability.value() != repository_capture.as_ref() {
        return Err(invalid("repository provenance binding mismatch"));
    }
    let sequence = u64::try_from(sequence).map_err(|_| invalid("negative observation sequence"))?;
    let observation: tracedecay_domain::DurableObservationV1 = decode(observation)?;
    let committed_cursor: ObservationSourceCursorV1 = decode(cursor)?;
    if observation.source() != committed_cursor.source()
        || observation.scope() != committed_cursor.scope()
        || observation.identity().generation() != committed_cursor.generation()
        || observation.identity().ordering_domain() != committed_cursor.ordering_domain()
        || observation.identity().position().end() != committed_cursor.position()
    {
        return Err(invalid("observation committed cursor binding mismatch"));
    }
    let retrieval_anchor: RetrievalAnchorRecordV2 = decode(
        retrieval_anchor.ok_or_else(|| invalid("observation retrieval anchor is missing"))?,
    )?;
    let projection_generation = ProjectionGenerationId::new(
        projection_generation
            .ok_or_else(|| invalid("observation projection generation is missing"))?,
    )
    .map_err(invalid)?;
    let repository_anchor = repository_anchor
        .map(decode::<RetrievalAnchorRecordV2>)
        .transpose()?;
    let expected_repository_owner = repository_anchor
        .as_ref()
        .map(|anchor| encode(anchor.owner()))
        .transpose()?;
    if repository_owner != expected_repository_owner {
        return Err(invalid("observation repository owner binding mismatch"));
    }
    let repository_provenance =
        RepositoryProvenanceAttachmentV1::new(repository_availability, repository_anchor)
            .map_err(invalid)?;
    ObservationCommitReceipt::new(
        sequence,
        observation.clone(),
        committed_cursor.clone(),
        retrieval_anchor.clone(),
        projection_generation.clone(),
    )
    .and_then(|receipt| {
        receipt.with_repository_provenance_attachment(repository_provenance.clone())
    })
    .map_err(invalid)?;
    Ok(StoredObservationRowV1 {
        sequence,
        observation,
        committed_cursor,
        retrieval_anchor,
        projection_generation,
        repository_provenance,
        projection_queued: projection_queued != 0,
    })
}

fn persist_sanitization_receipt(
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

fn cursor_advance_receipt_matches(
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

fn persist_retrieval_anchor(
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

fn persist_repository_provenance(
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

fn verify_observation_authority(
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

fn read_cursor(
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;
    use tracedecay_domain::{
        ComponentVersion, ObservationId, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId, RetentionClass,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
    };
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationCoverageReason, ObservationCursorAdvance,
        ObservationReadOperationV1, ObservationReadResultV1, ObservationWrite,
        SESSION_MESSAGE_PROJECTOR_VERSION, build_observation_resolution_authorization_v1,
        build_observation_retrieval_anchor_v2,
    };

    use super::ObservationExecutor;

    fn observation_write(body: &str, receipt_id: &str) -> ObservationWrite {
        observation_write_at(body, receipt_id, 1, 0, 1, None)
    }

    fn observation_write_at(
        body: &str,
        receipt_id: &str,
        generation: u64,
        start: u64,
        end: u64,
        expected_cursor: Option<ObservationSourceCursorV1>,
    ) -> ObservationWrite {
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new("provider.fixture").unwrap(),
            SessionId::new("session.fixture").unwrap(),
        )
        .unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: ProjectId::new("project.fixture").unwrap(),
        };
        let generation = ObservationSourceGenerationV1::new(generation).unwrap();
        let range = ObservationSourceRangeV1::new(start, end).unwrap();
        let payload = json!({"kind": "assistant_message", "body": body});
        let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(receipt_id).unwrap(),
                ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(payload_reference),
        )
        .unwrap();
        let observation = tracedecay_domain::DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                source.clone(),
                scope.clone(),
                generation,
                range,
                ObservationOrderingDomainV1::SqliteRowId,
                ObservationId::new("record.fixture").unwrap(),
            )
            .unwrap(),
            receipt,
            RetentionClass::new("retention.fixture").unwrap(),
            payload,
        )
        .unwrap();
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SqliteRowId,
            range.end(),
        )
        .unwrap();
        ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap()
    }

    fn anchored_observation_write(body: &str, receipt_id: &str) -> AnchoredObservationWrite {
        let write = observation_write(body, receipt_id);
        anchored(write)
    }

    fn anchored(write: ObservationWrite) -> AnchoredObservationWrite {
        let projection_generation = ProjectionGenerationId::new("projection.fixture.v1").unwrap();
        let authorization = build_observation_resolution_authorization_v1(
            write.observation(),
            "runtime.fixture.v1",
        )
        .unwrap();
        let anchor = build_observation_retrieval_anchor_v2(
            write.observation(),
            projection_generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
    }

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sanitization_receipts (
                    receipt_id TEXT PRIMARY KEY,
                    sanitizer_version TEXT NOT NULL,
                    payload_digest TEXT NOT NULL,
                    receipt_json TEXT NOT NULL
                 );
                 CREATE TABLE observations (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    observation_id TEXT NOT NULL UNIQUE,
                    payload_digest TEXT NOT NULL,
                    receipt_id TEXT NOT NULL,
                    observation_json TEXT NOT NULL,
                    committed_cursor_json TEXT NOT NULL
                 );
                 CREATE TABLE source_cursors (
                    source_json TEXT NOT NULL,
                    scope_json TEXT NOT NULL,
                    cursor_json TEXT NOT NULL,
                    PRIMARY KEY (source_json, scope_json)
                 );
                 CREATE TABLE source_cursor_advances (
                    source_json TEXT NOT NULL,
                    scope_json TEXT NOT NULL,
                    coverage_json TEXT NOT NULL,
                    reason TEXT NOT NULL,
                    receipt_id TEXT,
                    PRIMARY KEY(source_json, scope_json, coverage_json)
                 );
                 CREATE TABLE projection_queue (
                    observation_id TEXT PRIMARY KEY,
                    observation_sequence INTEGER NOT NULL UNIQUE
                 );
                 CREATE TABLE retrieval_anchors (
                    anchor_id TEXT PRIMARY KEY,
                    anchor_json TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    projection_generation TEXT NOT NULL,
                    UNIQUE(anchor_id, owner_json)
                 );
                 CREATE TABLE retrieval_anchor_aliases (
                    owner_json TEXT NOT NULL,
                    alias_kind TEXT NOT NULL,
                    locator_digest TEXT NOT NULL,
                    anchor_id TEXT NOT NULL,
                    PRIMARY KEY(owner_json, alias_kind, locator_digest)
                 );
                 CREATE TABLE observation_retrieval_anchors (
                    observation_id TEXT PRIMARY KEY,
                    anchor_id TEXT NOT NULL UNIQUE
                 );
                 CREATE TABLE observation_repository_provenance (
                    observation_id TEXT PRIMARY KEY,
                    availability_json TEXT NOT NULL,
                    capture_json TEXT,
                    retrieval_anchor_id TEXT UNIQUE,
                    owner_json TEXT
                 );
                 CREATE TABLE observation_projection_checkpoints (
                    projector_version TEXT PRIMARY KEY,
                    last_sequence INTEGER NOT NULL
                 );
                 CREATE TABLE observation_projection_rebuilds (
                    projector_version TEXT PRIMARY KEY,
                    generation TEXT NOT NULL,
                    frontier_sequence INTEGER NOT NULL,
                    aliases_staged_through INTEGER NOT NULL,
                    staged_through INTEGER NOT NULL,
                    projected_rows INTEGER NOT NULL,
                    skipped_observations INTEGER NOT NULL,
                    state TEXT NOT NULL
                 );",
            )
            .unwrap();
        connection
    }

    fn execute(
        connection: &mut Connection,
        write: &AnchoredObservationWrite,
    ) -> rusqlite::Result<()> {
        let mut transaction = connection.transaction()?;
        let savepoint = transaction.savepoint()?;
        ObservationExecutor.execute_write(&savepoint, write)?;
        savepoint.commit()?;
        transaction.commit()
    }

    fn read(
        connection: &mut Connection,
        operation: &ObservationReadOperationV1,
    ) -> rusqlite::Result<ObservationReadResultV1> {
        let transaction = connection.transaction()?;
        ObservationExecutor.execute_read(&transaction, operation)
    }

    fn execute_cursor_advance(
        connection: &mut Connection,
        advance: &ObservationCursorAdvance,
    ) -> rusqlite::Result<()> {
        let mut transaction = connection.transaction()?;
        let savepoint = transaction.savepoint()?;
        ObservationExecutor.execute_cursor_advance(&savepoint, advance)?;
        savepoint.commit()?;
        transaction.commit()
    }

    #[test]
    fn anchored_write_persists_all_authority_rows_atomically() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");

        execute(&mut connection, &write).unwrap();

        for table in [
            "observations",
            "sanitization_receipts",
            "retrieval_anchors",
            "retrieval_anchor_aliases",
            "observation_retrieval_anchors",
            "observation_repository_provenance",
            "source_cursors",
            "projection_queue",
        ] {
            let count = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            assert!(count > 0, "{table} was not persisted");
        }
    }

    #[test]
    fn relocated_native_duplicate_advances_coverage_without_reinserting() {
        let mut connection = connection();
        let original = anchored(observation_write_at(
            "stable payload",
            "receipt.original",
            1,
            41,
            42,
            None,
        ));
        execute(&mut connection, &original).unwrap();
        let relocated = anchored(observation_write_at(
            "stable payload",
            "receipt.relocated",
            2,
            71,
            72,
            Some(original.next_cursor().clone()),
        ));
        assert_eq!(
            original.observation().observation_id(),
            relocated.observation().observation_id()
        );

        execute(&mut connection, &relocated).unwrap();
        execute(&mut connection, &relocated).unwrap();

        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM observations", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM source_cursor_advances", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM sanitization_receipts", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
        let source_json = super::encode(relocated.observation().source()).unwrap();
        let scope_json = super::encode(relocated.observation().scope()).unwrap();
        assert_eq!(
            super::read_cursor(&connection, &source_json, &scope_json).unwrap(),
            Some(relocated.next_cursor().clone())
        );
    }

    #[test]
    fn exact_replay_is_a_no_op_after_the_source_cursor_advanced() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        let replay_write = ObservationWrite::new(
            write.observation().clone(),
            None,
            write.next_cursor().clone().with_resume_checkpoint(7, 11),
        )
        .unwrap();
        let replay = AnchoredObservationWrite::new(
            replay_write,
            write.retrieval_anchor().clone(),
            write.projection_generation().clone(),
        )
        .unwrap();

        execute(&mut connection, &write).unwrap();
        execute(&mut connection, &replay).unwrap();

        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM projection_queue", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn replay_with_different_anchor_fails_without_mutating_authority_rows() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();
        let conflicting_generation =
            ProjectionGenerationId::new("projection.conflicting.v1").unwrap();
        let authorization = build_observation_resolution_authorization_v1(
            write.observation(),
            "runtime.fixture.v1",
        )
        .unwrap();
        let conflicting_anchor = build_observation_retrieval_anchor_v2(
            write.observation(),
            conflicting_generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        let conflicting = AnchoredObservationWrite::new(
            ObservationWrite::new(
                write.observation().clone(),
                None,
                write.next_cursor().clone(),
            )
            .unwrap(),
            conflicting_anchor,
            conflicting_generation,
        )
        .unwrap();

        let error = execute(&mut connection, &conflicting).unwrap_err();

        assert!(error.to_string().contains("retrieval anchor"));
        for table in [
            "observations",
            "retrieval_anchors",
            "observation_retrieval_anchors",
            "observation_repository_provenance",
            "source_cursors",
            "projection_queue",
        ] {
            assert_eq!(
                connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                1,
                "{table} changed after rejected replay"
            );
        }
    }

    #[test]
    fn replay_does_not_repair_missing_anchor_authority() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();
        connection
            .execute("DELETE FROM retrieval_anchor_aliases", [])
            .unwrap();

        let error = execute(&mut connection, &write).unwrap_err();

        assert!(error.to_string().contains("retrieval anchor alias"));
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM retrieval_anchor_aliases", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn replay_rejects_extra_anchor_alias_authority() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();
        connection
            .execute(
                "INSERT INTO retrieval_anchor_aliases (
                    owner_json, alias_kind, locator_digest, anchor_id
                 )
                 SELECT owner_json, 'corrupt-extra', locator_digest, anchor_id
                 FROM retrieval_anchor_aliases LIMIT 1",
                [],
            )
            .unwrap();

        let error = execute(&mut connection, &write).unwrap_err();

        assert!(error.to_string().contains("retrieval anchor alias"));
    }

    #[test]
    fn identity_collision_fails_without_advancing_the_source_cursor() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();
        let cursor_before: String = connection
            .query_row("SELECT cursor_json FROM source_cursors", [], |row| {
                row.get(0)
            })
            .unwrap();

        let error = execute(
            &mut connection,
            &anchored_observation_write("conflicting", "receipt.conflicting"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("observation identity collision"));
        assert_eq!(
            connection
                .query_row("SELECT cursor_json FROM source_cursors", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            cursor_before
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM projection_queue", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn source_cursor_advance_replays_exactly_and_rejects_reason_collision() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();
        let advance = ObservationCursorAdvance::for_ordering(
            write.observation().source().clone(),
            write.observation().scope().clone(),
            write.observation().identity().generation(),
            write.observation().identity().ordering_domain(),
            Some(write.next_cursor().clone()),
            ObservationSourceRangeV1::new(1, 2).unwrap(),
            ObservationCoverageReason::BlankFrame,
        )
        .unwrap();

        execute_cursor_advance(&mut connection, &advance).unwrap();
        execute_cursor_advance(&mut connection, &advance).unwrap();

        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM source_cursor_advances", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        let conflicting = ObservationCursorAdvance::for_ordering(
            write.observation().source().clone(),
            write.observation().scope().clone(),
            write.observation().identity().generation(),
            write.observation().identity().ordering_domain(),
            Some(write.next_cursor().clone()),
            ObservationSourceRangeV1::new(1, 2).unwrap(),
            ObservationCoverageReason::OutOfScope,
        )
        .unwrap();
        let error = execute_cursor_advance(&mut connection, &conflicting).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("source cursor advance identity collision")
        );
    }

    #[test]
    fn retrieval_anchor_alias_reads_are_owner_bound() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        let alias = write.retrieval_anchor().aliases()[0].clone();
        execute(&mut connection, &write).unwrap();

        let resolved = read(
            &mut connection,
            &ObservationReadOperationV1::RetrievalAnchorByAlias {
                scope: write.observation().scope().clone(),
                alias: alias.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            resolved,
            ObservationReadResultV1::RetrievalAnchorByAlias(Some(
                write.retrieval_anchor_id().clone()
            ))
        );

        let foreign = read(
            &mut connection,
            &ObservationReadOperationV1::RetrievalAnchorByAlias {
                scope: ObservationScopeV1::Profile,
                alias,
            },
        )
        .unwrap();
        assert_eq!(
            foreign,
            ObservationReadResultV1::RetrievalAnchorByAlias(None)
        );
    }

    #[test]
    fn replay_queue_and_checkpoint_reads_preserve_projection_ordering() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();

        let point = read(
            &mut connection,
            &ObservationReadOperationV1::Observation {
                observation_id: write.observation().observation_id().clone(),
            },
        )
        .unwrap();
        let ObservationReadResultV1::Observation(point) = point else {
            panic!("unexpected point-read result");
        };
        let point = point.expect("persisted observation must be readable");
        assert_eq!(point.observation, *write.observation());
        assert_eq!(point.committed_cursor, *write.next_cursor());
        assert_eq!(point.retrieval_anchor, *write.retrieval_anchor());
        assert_eq!(point.projection_generation, *write.projection_generation());
        assert_eq!(
            point.repository_provenance,
            *write.repository_provenance_attachment()
        );
        assert!(point.projection_queued);

        let replay = read(
            &mut connection,
            &ObservationReadOperationV1::Replay {
                after_sequence: 0,
                limit: 10,
            },
        )
        .unwrap();
        let ObservationReadResultV1::Replay(rows) = replay else {
            panic!("unexpected replay result");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].observation.observation_id(),
            write.observation().observation_id()
        );
        assert_eq!(&rows[0].retrieval_anchor, write.retrieval_anchor());
        assert_eq!(
            &rows[0].projection_generation,
            write.projection_generation()
        );
        assert_eq!(
            &rows[0].repository_provenance,
            write.repository_provenance_attachment()
        );
        assert!(rows[0].projection_queued);

        assert_eq!(
            read(
                &mut connection,
                &ObservationReadOperationV1::NextQueuedProjection,
            )
            .unwrap(),
            ObservationReadResultV1::NextQueuedProjection(Some(
                write.observation().observation_id().clone()
            ))
        );
        assert_eq!(
            read(
                &mut connection,
                &ObservationReadOperationV1::ProjectionCheckpoint,
            )
            .unwrap(),
            ObservationReadResultV1::ProjectionCheckpoint(0)
        );

        connection
            .execute(
                "INSERT INTO observation_projection_checkpoints
                    (projector_version, last_sequence) VALUES (?1, 1)",
                [SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO observation_projection_rebuilds (
                    projector_version, generation, frontier_sequence, aliases_staged_through,
                    staged_through, projected_rows, skipped_observations, state
                 ) VALUES (?1, ?2, 1, 1, 1, 1, 0, 'ready')",
                [SESSION_MESSAGE_PROJECTOR_VERSION, "projection.fixture.v1"],
            )
            .unwrap();
        assert_eq!(
            read(
                &mut connection,
                &ObservationReadOperationV1::NextQueuedProjection,
            )
            .unwrap(),
            ObservationReadResultV1::NextQueuedProjection(None)
        );
        assert_eq!(
            read(
                &mut connection,
                &ObservationReadOperationV1::ProjectionCheckpoint,
            )
            .unwrap(),
            ObservationReadResultV1::ProjectionCheckpoint(1)
        );
        let progress = read(
            &mut connection,
            &ObservationReadOperationV1::ProjectionRebuildProgress,
        )
        .unwrap();
        let ObservationReadResultV1::ProjectionRebuildProgress(Some(progress)) = progress else {
            panic!("unexpected projection rebuild progress result");
        };
        assert_eq!(
            progress.generation,
            ProjectionGenerationId::new("projection.fixture.v1").unwrap()
        );
        assert_eq!(progress.frontier_sequence, 1);
        assert_eq!(progress.staged_through, 1);
        assert_eq!(progress.projected_rows, 1);
    }

    #[test]
    fn point_and_replay_reads_reject_incomplete_observation_authority() {
        let mut connection = connection();
        let write = anchored_observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();
        connection
            .execute("DELETE FROM observation_retrieval_anchors", [])
            .unwrap();

        for operation in [
            ObservationReadOperationV1::Observation {
                observation_id: write.observation().observation_id().clone(),
            },
            ObservationReadOperationV1::Replay {
                after_sequence: 0,
                limit: 10,
            },
        ] {
            let error = read(&mut connection, &operation).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("observation retrieval anchor is missing")
            );
        }
    }
}
