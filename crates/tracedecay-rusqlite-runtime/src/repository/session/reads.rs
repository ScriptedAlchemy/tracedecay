//! The session read operations, and the per-table reads a batch read composes.

use rusqlite::{OptionalExtension, params};
use tracedecay_domain::{
    AgentInstanceId, CanonicalObservationIdV1, GroupingProvenanceV1, MessageId,
    MessageOccurrenceIdV1, MessageOccurrenceRecordV1, ProjectionOutputOrdinalV1, RetrievalAnchorId,
    SessionId, SessionProjectionGenerationV1, TemporalAssertionIdV1, TemporalAssertionKindV1,
    TemporalAssertionRecordV1, TemporalValidityV1, ThreadId, TurnId, UtcMicros,
};
use tracedecay_store::SessionTemporalProjectionBatchV1;

use super::super::support::{decode, invalid, u64_to_i64};
use super::projection::decode_watermarks;

pub(super) fn read_projection_batch(
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
    let assertions = read_assertions(connection, session_id, generation_value)?;
    SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation,
        decode_watermarks(watermarks)?,
        occurrences,
        Vec::new(),
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
