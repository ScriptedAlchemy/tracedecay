//! Writing and reading one session's temporal projection and summaries.
//!
//! The executor owns the transaction shape; the siblings own the pieces it
//! composes — [`projection`] the prepared statements and watermark/digest
//! encodings a batch writes through, and [`reads`] the read operations.

use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_store::{
    SessionReadOperationV1, SessionReadResultV1, SessionSummaryPublicationRequestV1,
    SessionTemporalProjectionBatchV1,
};

use super::support::{canonical_digest, encode, invalid, u64_to_i64, usize_to_i64};

mod projection;
mod reads;

use projection::{ProjectionStatements, encode_watermarks, insert_occurrence, projection_digest};
use reads::read_projection_batch;

#[derive(Clone, Default)]
pub struct SessionExecutor;

impl SessionExecutor {
    pub fn execute_projection_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        batch: &SessionTemporalProjectionBatchV1,
    ) -> rusqlite::Result<()> {
        if !batch.copies().is_empty() {
            return Err(invalid(
                "session logical-copy relations are owned by the native graph store",
            ));
        }
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

        let mut statements = ProjectionStatements {
            thread: savepoint.prepare(
                "INSERT OR IGNORE INTO session_threads (
                    session_id, generation, thread_id, grouping_provenance, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?,
            turn: savepoint.prepare(
                "INSERT OR IGNORE INTO session_turns (
                    session_id, generation, turn_id, ordinal, grouping_provenance, created_at
                 ) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
            )?,
            agent: savepoint.prepare(
                "INSERT OR IGNORE INTO session_agents (
                    session_id, generation, agent_id, agent_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?,
            occurrence: savepoint.prepare(
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
            )?,
            assertion: savepoint.prepare(
                "INSERT INTO session_assertions (
                    session_id, generation, assertion_id, assertion_kind,
                    subject_anchor_id, object_anchor_id, knowledge_at,
                    valid_time_json, evidence_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?,
            receipt: savepoint.prepare(
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
            )?,
        };
        for occurrence in batch.occurrences() {
            insert_occurrence(&mut statements, batch, generation, occurrence)?;
        }
        for assertion in batch.assertions() {
            statements.assertion.execute(params![
                batch.session_id().as_str(),
                generation,
                assertion.assertion_id.as_str(),
                assertion.kind.as_str(),
                assertion.subject_anchor_id.as_str(),
                assertion.object_anchor_id.as_str(),
                assertion.knowledge_at.0,
                encode(&assertion.valid_time)?,
                encode(&assertion.evidence)?,
            ])?;
        }

        let occurrence_digest = canonical_digest(batch.occurrences())?;
        let copy_digest = canonical_digest(&Vec::<String>::new())?;
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
        statements.receipt.execute(params![
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
            0,
            copy_digest,
            usize_to_i64(batch.assertions().len(), "session assertion count")?,
            assertion_digest,
            committed_at,
        ])?;
        Ok(())
    }

    pub fn execute_summary_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        request: &SessionSummaryPublicationRequestV1,
    ) -> rusqlite::Result<()> {
        let _ = (savepoint, request);
        Err(invalid(
            "session summary relations are owned by the native graph store",
        ))
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
            SessionReadOperationV1::Summary(_) => Err(invalid(
                "session summary relations are owned by the native graph store",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
