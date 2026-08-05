//! The prepared statements and encodings one projection batch writes through.
//!
//! [`ProjectionStatements`] is prepared once per batch so a batch of any size
//! costs one prepare per table, and the watermark codec lives beside it because
//! the frozen watermarks are part of both the generation check and the digest.

use rusqlite::{Statement, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracedecay_domain::{
    MessageOccurrenceRecordV1, SessionProjectionGenerationV1, SignedCursorKeyRefV1,
};
use tracedecay_store::{SessionFrozenWatermarksV1, SessionTemporalProjectionBatchV1};

use super::super::support::{canonical_digest, decode, encode, invalid, sha256_hex, usize_to_i64};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredWatermarksV1 {
    active_generation: SessionProjectionGenerationV1,
    source_frontier: u64,
    projection_frontier: u64,
    summary_frontier: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
}

pub(super) fn encode_watermarks(
    watermarks: &SessionFrozenWatermarksV1,
) -> rusqlite::Result<String> {
    encode(&StoredWatermarksV1 {
        active_generation: watermarks.active_generation(),
        source_frontier: watermarks.source_frontier(),
        projection_frontier: watermarks.projection_frontier(),
        summary_frontier: watermarks.summary_frontier(),
        cursor_key: watermarks.cursor_key().cloned(),
    })
}

pub(super) fn decode_watermarks(value: String) -> rusqlite::Result<SessionFrozenWatermarksV1> {
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

pub(super) fn projection_digest(
    batch: &SessionTemporalProjectionBatchV1,
) -> rusqlite::Result<String> {
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

pub(super) struct ProjectionStatements<'connection> {
    pub(super) thread: Statement<'connection>,
    pub(super) turn: Statement<'connection>,
    pub(super) agent: Statement<'connection>,
    pub(super) message: Statement<'connection>,
    pub(super) occurrence: Statement<'connection>,
    pub(super) copy: Statement<'connection>,
    pub(super) assertion: Statement<'connection>,
    pub(super) receipt: Statement<'connection>,
}

pub(super) fn insert_occurrence(
    statements: &mut ProjectionStatements<'_>,
    batch: &SessionTemporalProjectionBatchV1,
    generation: i64,
    occurrence: &MessageOccurrenceRecordV1,
) -> rusqlite::Result<()> {
    if let (Some(thread_id), Some(grouping)) = (&occurrence.thread_id, &occurrence.thread_grouping)
    {
        statements.thread.execute(params![
            batch.session_id().as_str(),
            generation,
            thread_id.as_str(),
            encode(grouping)?,
            occurrence.knowledge_at.0,
        ])?;
    }
    if let (Some(turn_id), Some(grouping)) = (&occurrence.turn_id, &occurrence.turn_grouping) {
        statements.turn.execute(params![
            batch.session_id().as_str(),
            generation,
            turn_id.as_str(),
            encode(grouping)?,
            occurrence.knowledge_at.0,
        ])?;
    }
    if let Some(agent_id) = &occurrence.agent_id {
        statements.agent.execute(params![
            batch.session_id().as_str(),
            generation,
            agent_id.as_str(),
            encode(agent_id)?,
            occurrence.knowledge_at.0,
        ])?;
    }
    let role = encode(&occurrence.role)?.trim_matches('"').to_owned();
    let message_id = occurrence
        .message_id
        .as_ref()
        .ok_or_else(|| invalid("session occurrence has no message id"))?;
    let (source_provider, sanitized_content) = {
        let mut rows = statements
            .message
            .query(params![batch.session_id().as_str(), message_id.as_str()])?;
        let row = rows
            .next()?
            .ok_or_else(|| invalid("session occurrence has no canonical message projection"))?;
        let source_provider = row.get::<_, String>(0)?;
        let projected_role = row.get::<_, String>(1)?;
        let sanitized_content = row.get::<_, String>(2)?;
        if source_provider.is_empty() || projected_role != role || rows.next()?.is_some() {
            return Err(invalid(
                "session occurrence canonical message projection is ambiguous or mismatched",
            ));
        }
        (source_provider, sanitized_content)
    };
    let sanitized_content_digest = sha256_hex(sanitized_content.as_bytes());
    let sanitized_content_bytes = usize_to_i64(sanitized_content.len(), "sanitized content bytes")?;
    statements.occurrence.execute(params![
        batch.session_id().as_str(),
        generation,
        occurrence.occurrence_id.as_str(),
        occurrence.source_observation_id.as_str(),
        source_provider,
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
        sanitized_content_digest,
        sanitized_content_bytes,
    ])?;
    Ok(())
}
