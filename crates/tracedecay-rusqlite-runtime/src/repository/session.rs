use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracedecay_domain::{
    AgentInstanceId, CanonicalObservationIdV1, CopyProofV1, GroupingProvenanceV1,
    LogicalCopyRecordV1, MessageId, MessageOccurrenceIdV1, MessageOccurrenceRecordV1,
    ProjectionOutputOrdinalV1, RetrievalAnchorId, SessionId, SessionProjectionGenerationV1,
    SessionSummaryIdV1, SessionSummaryRecordV1, SignedCursorKeyRefV1, SummaryPublicationMetadataV1,
    SummarySourceHorizonV1, TemporalAssertionIdV1, TemporalAssertionKindV1,
    TemporalAssertionRecordV1, TemporalValidityV1, ThreadId, TurnId, UtcMicros,
};
use tracedecay_store::{
    SessionFrozenWatermarksV1, SessionReadOperationV1, SessionReadResultV1,
    SessionTemporalProjectionBatchV1,
};

use super::support::{canonical_digest, decode, encode, invalid, u64_to_i64, usize_to_i64};

#[derive(Clone, Default)]
pub struct SessionExecutor;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredWatermarksV1 {
    active_generation: SessionProjectionGenerationV1,
    source_frontier: u64,
    projection_frontier: u64,
    summary_frontier: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
}

impl SessionExecutor {
    pub fn execute_projection_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        batch: &SessionTemporalProjectionBatchV1,
    ) -> rusqlite::Result<()> {
        let generation = u64_to_i64(batch.generation().value(), "session generation")?;
        let frozen = encode_watermarks(batch.watermarks())?;
        let stored_generation = savepoint
            .query_row(
                "SELECT state, frozen_watermarks_json
                 FROM session_temporal_generations
                 WHERE session_id = ?1 AND generation = ?2",
                params![batch.session_id().as_str(), generation],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| invalid("session projection generation is missing"))?;
        if stored_generation.0 != "building" || stored_generation.1 != frozen {
            return Err(invalid(
                "session projection generation is not the matching building generation",
            ));
        }

        let digest = projection_digest(batch)?;
        let existing = savepoint
            .query_row(
                "SELECT batch_digest FROM session_temporal_projection_receipts
                 WHERE session_id = ?1 AND generation = ?2 AND batch_ordinal = ?3",
                params![
                    batch.session_id().as_str(),
                    generation,
                    u64_to_i64(batch.batch_ordinal(), "session batch ordinal")?,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            return if existing == digest {
                Ok(())
            } else {
                Err(invalid("session projection batch identity conflict"))
            };
        }

        for occurrence in batch.occurrences() {
            insert_occurrence(savepoint, batch, occurrence)?;
        }
        for copy in batch.copies() {
            savepoint.execute(
                "INSERT INTO session_logical_copy_edges (
                    session_id, generation, occurrence_id, copied_from_occurrence_id,
                    proof_json, knowledge_at, valid_time_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    batch.session_id().as_str(),
                    generation,
                    copy.occurrence_id.as_str(),
                    copy.copied_from_occurrence_id.as_str(),
                    encode(&copy.proof)?,
                    copy.knowledge_at.0,
                    encode(&copy.valid_time)?,
                    copy.knowledge_at.0,
                ],
            )?;
        }
        for assertion in batch.assertions() {
            savepoint.execute(
                "INSERT INTO session_assertions (
                    session_id, generation, assertion_id, assertion_kind,
                    subject_anchor_id, object_anchor_id, knowledge_at,
                    valid_time_json, evidence_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    batch.session_id().as_str(),
                    generation,
                    assertion.assertion_id.as_str(),
                    assertion.kind.as_str(),
                    assertion.subject_anchor_id.as_str(),
                    assertion.object_anchor_id.as_str(),
                    assertion.knowledge_at.0,
                    encode(&assertion.valid_time)?,
                    encode(&assertion.evidence)?,
                ],
            )?;
        }

        let occurrence_digest = canonical_digest(batch.occurrences())?;
        let copy_digest = canonical_digest(batch.copies())?;
        let assertion_digest = canonical_digest(batch.assertions())?;
        let empty_digest = canonical_digest(&Vec::<String>::new())?;
        let committed_at = batch
            .occurrences()
            .iter()
            .map(|record| record.knowledge_at.0)
            .chain(batch.copies().iter().map(|record| record.knowledge_at.0))
            .chain(
                batch
                    .assertions()
                    .iter()
                    .map(|record| record.knowledge_at.0),
            )
            .max()
            .unwrap_or(0);
        savepoint.execute(
            "INSERT INTO session_temporal_projection_receipts (
                session_id, generation, batch_ordinal, batch_digest,
                frozen_watermarks_json, source_through, projection_through,
                occurrence_count, occurrence_digest, dimension_count, dimension_digest,
                copy_count, copy_digest, assertion_count, assertion_digest,
                supersession_count, supersession_digest, current_count, current_digest,
                fts_count, fts_digest, committed_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10,
                ?11, ?12, ?13, ?14, 0, ?10, 0, ?10, 0, ?10, ?15
             )",
            params![
                batch.session_id().as_str(),
                generation,
                u64_to_i64(batch.batch_ordinal(), "session batch ordinal")?,
                digest,
                frozen,
                u64_to_i64(batch.source_through(), "session source frontier")?,
                u64_to_i64(batch.projection_through(), "session projection frontier")?,
                usize_to_i64(batch.occurrences().len(), "session occurrence count")?,
                occurrence_digest,
                empty_digest,
                usize_to_i64(batch.copies().len(), "session copy count")?,
                copy_digest,
                usize_to_i64(batch.assertions().len(), "session assertion count")?,
                assertion_digest,
                committed_at,
            ],
        )?;
        Ok(())
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &SessionReadOperationV1,
    ) -> rusqlite::Result<SessionReadResultV1> {
        match operation {
            SessionReadOperationV1::ProjectionBatch {
                session_id,
                generation,
                batch_ordinal,
            } => read_projection_batch(snapshot, session_id, *generation, *batch_ordinal)
                .map(SessionReadResultV1::ProjectionBatch),
            SessionReadOperationV1::Summary(summary_id) => {
                read_summary(snapshot, summary_id).map(SessionReadResultV1::Summary)
            }
        }
    }
}

fn encode_watermarks(watermarks: &SessionFrozenWatermarksV1) -> rusqlite::Result<String> {
    encode(&StoredWatermarksV1 {
        active_generation: watermarks.active_generation(),
        source_frontier: watermarks.source_frontier(),
        projection_frontier: watermarks.projection_frontier(),
        summary_frontier: watermarks.summary_frontier(),
        cursor_key: watermarks.cursor_key().cloned(),
    })
}

fn decode_watermarks(value: String) -> rusqlite::Result<SessionFrozenWatermarksV1> {
    let stored = decode::<StoredWatermarksV1>(value)?;
    let watermarks = SessionFrozenWatermarksV1::new(
        stored.active_generation,
        stored.source_frontier,
        stored.projection_frontier,
        stored.summary_frontier,
    );
    Ok(match stored.cursor_key {
        Some(cursor_key) => watermarks.with_cursor_key(cursor_key),
        None => watermarks,
    })
}

fn projection_digest(batch: &SessionTemporalProjectionBatchV1) -> rusqlite::Result<String> {
    canonical_digest(&json!({
        "session_id": batch.session_id(),
        "generation": batch.generation(),
        "watermarks": encode_watermarks(batch.watermarks())?,
        "batch_ordinal": batch.batch_ordinal(),
        "source_through": batch.source_through(),
        "projection_through": batch.projection_through(),
        "occurrences": batch.occurrences(),
        "copies": batch.copies(),
        "assertions": batch.assertions(),
    }))
}

fn insert_occurrence(
    savepoint: &Savepoint<'_>,
    batch: &SessionTemporalProjectionBatchV1,
    occurrence: &MessageOccurrenceRecordV1,
) -> rusqlite::Result<()> {
    let generation = u64_to_i64(batch.generation().value(), "session generation")?;
    if let (Some(thread_id), Some(grouping)) = (&occurrence.thread_id, &occurrence.thread_grouping)
    {
        savepoint.execute(
            "INSERT OR IGNORE INTO session_threads (
                session_id, generation, thread_id, grouping_provenance, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                batch.session_id().as_str(),
                generation,
                thread_id.as_str(),
                encode(grouping)?,
                occurrence.knowledge_at.0,
            ],
        )?;
    }
    if let (Some(turn_id), Some(grouping)) = (&occurrence.turn_id, &occurrence.turn_grouping) {
        savepoint.execute(
            "INSERT OR IGNORE INTO session_turns (
                session_id, generation, turn_id, ordinal, grouping_provenance, created_at
             ) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
            params![
                batch.session_id().as_str(),
                generation,
                turn_id.as_str(),
                encode(grouping)?,
                occurrence.knowledge_at.0,
            ],
        )?;
    }
    if let Some(agent_id) = &occurrence.agent_id {
        savepoint.execute(
            "INSERT OR IGNORE INTO session_agents (
                session_id, generation, agent_id, agent_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                batch.session_id().as_str(),
                generation,
                agent_id.as_str(),
                encode(agent_id)?,
                occurrence.knowledge_at.0,
            ],
        )?;
    }
    let role = encode(&occurrence.role)?.trim_matches('"').to_owned();
    savepoint.execute(
        "INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, thread_id,
            thread_grouping_json, turn_id, turn_grouping_json, message_id,
            agent_id, role, knowledge_at, valid_time_json, evidence_json,
            snippet_text, index_text
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, '', ''
         )",
        params![
            batch.session_id().as_str(),
            generation,
            occurrence.occurrence_id.as_str(),
            occurrence.source_observation_id.as_str(),
            i64::from(occurrence.projection_output_ordinal.value()),
            occurrence.retrieval_anchor_id.as_str(),
            occurrence.thread_id.as_ref().map(|value| value.as_str()),
            occurrence
                .thread_grouping
                .as_ref()
                .map(encode)
                .transpose()?,
            occurrence.turn_id.as_ref().map(|value| value.as_str()),
            occurrence.turn_grouping.as_ref().map(encode).transpose()?,
            occurrence.message_id.as_ref().map(|value| value.as_str()),
            occurrence.agent_id.as_ref().map(|value| value.as_str()),
            role,
            occurrence.knowledge_at.0,
            encode(&occurrence.valid_time)?,
            encode(&occurrence.evidence)?,
        ],
    )?;
    Ok(())
}

fn read_projection_batch(
    connection: &rusqlite::Connection,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
) -> rusqlite::Result<Option<SessionTemporalProjectionBatchV1>> {
    let generation_value = u64_to_i64(generation.value(), "session generation")?;
    let receipt = connection
        .query_row(
            "SELECT frozen_watermarks_json, source_through, projection_through
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2 AND batch_ordinal = ?3",
            params![
                session_id.as_str(),
                generation_value,
                u64_to_i64(batch_ordinal, "session batch ordinal")?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((watermarks, source_through, projection_through)) = receipt else {
        return Ok(None);
    };
    let occurrences = read_occurrences(connection, session_id, generation_value)?;
    let copies = read_copies(connection, session_id, generation_value)?;
    let assertions = read_assertions(connection, session_id, generation_value)?;
    SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation,
        decode_watermarks(watermarks)?,
        occurrences,
        copies,
        assertions,
    )
    .and_then(|batch| {
        batch.with_checkpoint(
            batch_ordinal,
            u64::try_from(source_through).map_err(|_| {
                tracedecay_store::SessionStoreError::InvalidStateTransition {
                    context: "stored source frontier",
                }
            })?,
            u64::try_from(projection_through).map_err(|_| {
                tracedecay_store::SessionStoreError::InvalidStateTransition {
                    context: "stored projection frontier",
                }
            })?,
        )
    })
    .map(Some)
    .map_err(invalid)
}

fn read_occurrences(
    connection: &rusqlite::Connection,
    session_id: &SessionId,
    generation: i64,
) -> rusqlite::Result<Vec<MessageOccurrenceRecordV1>> {
    let mut statement = connection.prepare(
        "SELECT occurrence_id, source_observation_id, projection_output_ordinal,
                retrieval_anchor_id, thread_id, thread_grouping_json,
                turn_id, turn_grouping_json, message_id, agent_id, role,
                knowledge_at, valid_time_json, evidence_json
         FROM session_occurrences
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY knowledge_at, occurrence_id",
    )?;
    let mut records = Vec::new();
    let mut rows = statement.query(params![session_id.as_str(), generation])?;
    while let Some(row) = rows.next()? {
        let ordinal = row.get::<_, i64>(2)?;
        let knowledge_at = row.get::<_, i64>(11)?;
        if ordinal < 0 {
            return Err(invalid("negative projection output ordinal"));
        }
        let record = MessageOccurrenceRecordV1 {
            occurrence_id: MessageOccurrenceIdV1::new(row.get::<_, String>(0)?).map_err(invalid)?,
            source_observation_id: CanonicalObservationIdV1::new(row.get::<_, String>(1)?)
                .map_err(invalid)?,
            projection_output_ordinal: ProjectionOutputOrdinalV1::new(ordinal as u32),
            retrieval_anchor_id: RetrievalAnchorId::new(row.get::<_, String>(3)?)
                .map_err(invalid)?,
            session_id: session_id.clone(),
            thread_id: row
                .get::<_, Option<String>>(4)?
                .map(ThreadId::new)
                .transpose()
                .map_err(invalid)?,
            thread_grouping: row
                .get::<_, Option<String>>(5)?
                .map(decode::<GroupingProvenanceV1>)
                .transpose()?,
            turn_id: row
                .get::<_, Option<String>>(6)?
                .map(TurnId::new)
                .transpose()
                .map_err(invalid)?,
            turn_grouping: row
                .get::<_, Option<String>>(7)?
                .map(decode::<GroupingProvenanceV1>)
                .transpose()?,
            message_id: row
                .get::<_, Option<String>>(8)?
                .map(MessageId::new)
                .transpose()
                .map_err(invalid)?,
            agent_id: row
                .get::<_, Option<String>>(9)?
                .map(AgentInstanceId::new)
                .transpose()
                .map_err(invalid)?,
            role: decode(format!("\"{}\"", row.get::<_, String>(10)?))?,
            knowledge_at: UtcMicros(knowledge_at),
            valid_time: decode::<TemporalValidityV1>(row.get::<_, String>(12)?)?,
            evidence: decode(row.get::<_, String>(13)?)?,
        };
        record.validate().map_err(invalid)?;
        records.push(record);
    }
    Ok(records)
}

fn read_copies(
    connection: &rusqlite::Connection,
    session_id: &SessionId,
    generation: i64,
) -> rusqlite::Result<Vec<LogicalCopyRecordV1>> {
    let mut statement = connection.prepare(
        "SELECT occurrence_id, copied_from_occurrence_id, proof_json,
                knowledge_at, valid_time_json
         FROM session_logical_copy_edges
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY knowledge_at, occurrence_id, copied_from_occurrence_id",
    )?;
    statement
        .query_map(params![session_id.as_str(), generation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .map(|row| {
            let (occurrence, source, proof, knowledge, valid_time) = row?;
            let record = LogicalCopyRecordV1 {
                occurrence_id: MessageOccurrenceIdV1::new(occurrence).map_err(invalid)?,
                copied_from_occurrence_id: MessageOccurrenceIdV1::new(source).map_err(invalid)?,
                proof: decode::<CopyProofV1>(proof)?,
                knowledge_at: UtcMicros(knowledge),
                valid_time: decode::<TemporalValidityV1>(valid_time)?,
            };
            record.validate().map_err(invalid)?;
            Ok(record)
        })
        .collect()
}

fn read_assertions(
    connection: &rusqlite::Connection,
    session_id: &SessionId,
    generation: i64,
) -> rusqlite::Result<Vec<TemporalAssertionRecordV1>> {
    let mut statement = connection.prepare(
        "SELECT assertion_id, assertion_kind, subject_anchor_id, object_anchor_id,
                knowledge_at, valid_time_json, evidence_json
         FROM session_assertions
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY knowledge_at, assertion_id",
    )?;
    statement
        .query_map(params![session_id.as_str(), generation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .map(|row| {
            let (id, kind, subject, object, knowledge, valid_time, evidence) = row?;
            let record = TemporalAssertionRecordV1 {
                assertion_id: TemporalAssertionIdV1::new(id).map_err(invalid)?,
                kind: decode::<TemporalAssertionKindV1>(format!("\"{kind}\""))?,
                subject_anchor_id: RetrievalAnchorId::new(subject).map_err(invalid)?,
                object_anchor_id: RetrievalAnchorId::new(object).map_err(invalid)?,
                knowledge_at: UtcMicros(knowledge),
                valid_time: decode(valid_time)?,
                evidence: decode(evidence)?,
            };
            record.validate().map_err(invalid)?;
            Ok(record)
        })
        .collect()
}

fn read_summary(
    connection: &rusqlite::Connection,
    summary_id: &SessionSummaryIdV1,
) -> rusqlite::Result<Option<SessionSummaryRecordV1>> {
    let row = connection
        .query_row(
            "SELECT session_id, summary_anchor_id, source_horizon_json,
                    publication_json, created_at
             FROM session_summary_nodes WHERE summary_id = ?1",
            [summary_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((session_id, anchor_id, horizon, publication, created_at)) = row else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT source_anchor_id FROM session_summary_sources
         WHERE summary_id = ?1 AND source_kind = 'anchor'
         ORDER BY source_ordinal",
    )?;
    let sources = statement
        .query_map([summary_id.as_str()], |row| row.get::<_, String>(0))?
        .map(|row| RetrievalAnchorId::new(row?).map_err(invalid))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut summary = SessionSummaryRecordV1::new(
        summary_id.clone(),
        SessionId::new(session_id).map_err(invalid)?,
        RetrievalAnchorId::new(anchor_id).map_err(invalid)?,
        sources,
        decode::<SummarySourceHorizonV1>(horizon)?,
        UtcMicros(created_at),
    )
    .map_err(invalid)?;
    let predecessor = connection
        .query_row(
            "SELECT predecessor_summary_id
             FROM session_summary_successors
             WHERE successor_summary_id = ?1
             ORDER BY created_at, predecessor_summary_id",
            [summary_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(predecessor) = predecessor {
        summary = summary
            .with_predecessor(SessionSummaryIdV1::new(predecessor).map_err(invalid)?)
            .map_err(invalid)?;
    }
    if let Some(publication) = publication {
        summary = summary
            .with_publication(decode::<SummaryPublicationMetadataV1>(publication)?)
            .map_err(invalid)?;
    }
    Ok(Some(summary))
}
