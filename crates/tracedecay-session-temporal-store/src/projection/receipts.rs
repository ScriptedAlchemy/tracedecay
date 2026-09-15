use std::{collections::BTreeSet, sync::Arc};

use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_domain::RetrievalAnchorRecord;
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::engine::{Executor, params};
use tracedecay_store::{
    ObservationProjection, ProjectionStoreError, ProjectionStoreResult, SessionStoreResult,
    SessionTemporalDigestV1, SessionTemporalProjectionBatchReceiptV1,
    SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use super::super::query::{
    PERSIST_OPERATION, encode_watermarks, frontier_i64, generation_i64, storage, storage_message,
};
use super::super::rebuild::checkpoint_relation_rebuild_control;
use super::super::relation_projection::reconstruct_logical_copy_relations;
use super::super::relations::{LogicalCopyRelation, SessionRelationProjection};
use super::persist::*;

const MAX_RECEIPT_COPY_ENTITIES: usize = 100_000;

#[hotpath::measure(future = true, label = "session_temporal.projection.validate_receipt")]
pub async fn validate_final_projection_receipt(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &tracedecay_domain::SessionId,
    generation: tracedecay_domain::SessionProjectionGenerationV1,
    watermarks: &tracedecay_store::SessionFrozenWatermarksV1,
    relation_projection: &SessionRelationProjection,
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    checkpoint_relation_rebuild_control(control)?;
    let generation_i64 = generation_i64(generation, super::super::query::ACTIVATE_OPERATION)?;
    let mut rows = conn
        .query(
            "SELECT COUNT(*), MIN(batch_ordinal), MAX(batch_ordinal)
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id.as_str(), generation_i64],
        )
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                super::super::query::ACTIVATE_OPERATION,
                "projection receipt aggregate returned no row",
            )
        })?;
    let count: i64 = row
        .get(0)
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    let minimum: Option<i64> = row
        .get(1)
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    let maximum: Option<i64> = row
        .get(2)
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    drop(rows);
    if count <= 0 || minimum != Some(0) || maximum != Some(count - 1) {
        return Err(storage_message(
            super::super::query::ACTIVATE_OPERATION,
            "candidate projection receipts are missing or noncontiguous",
        ));
    }
    let mut rows = conn
        .query(
            "SELECT source_through, projection_through,
                    occurrence_count, occurrence_digest,
                    dimension_count, dimension_digest,
                    copy_count, copy_digest,
                    assertion_count, assertion_digest,
                    supersession_count, supersession_digest,
                    current_count, current_digest,
                    fts_count, fts_digest
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2
             ORDER BY batch_ordinal DESC LIMIT 1",
            params![session_id.as_str(), generation_i64],
        )
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                super::super::query::ACTIVATE_OPERATION,
                "candidate final projection receipt is missing",
            )
        })?;
    let source_through: i64 = row
        .get(0)
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    let projection_through: i64 = row
        .get(1)
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    if u64::try_from(source_through).ok() != Some(watermarks.source_frontier())
        || u64::try_from(projection_through).ok() != Some(watermarks.projection_frontier())
    {
        return Err(storage_message(
            super::super::query::ACTIVATE_OPERATION,
            "final projection receipt does not cover the frozen frontiers",
        ));
    }
    let count = |index| -> SessionStoreResult<usize> {
        let value = row
            .get::<i64>(index)
            .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
        usize::try_from(value)
            .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))
    };
    let digest = |index| {
        row.get::<String>(index)
            .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))
    };
    let expected = ProjectionCoverage {
        occurrences: (count(2)?, digest(3)?),
        dimensions: (count(4)?, digest(5)?),
        copies: (count(6)?, digest(7)?),
        assertions: (count(8)?, digest(9)?),
        supersession: (count(10)?, digest(11)?),
        current: (count(12)?, digest(13)?),
        fts: (count(14)?, digest(15)?),
    };
    let batch = SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation,
        watermarks.clone(),
        vec![],
        vec![],
        vec![],
    )?;
    let actual = projection_coverage(conn, &batch, Some(control)).await?;
    if actual != expected {
        return Err(storage_message(
            super::super::query::ACTIVATE_OPERATION,
            "candidate projection rows do not match the immutable final receipt",
        ));
    }
    if copy_identity_coverage(&relation_projection.logical_copies)? != expected.copies {
        return Err(storage_message(
            super::super::query::ACTIVATE_OPERATION,
            "native relation graph copies do not match the immutable final receipt",
        ));
    }
    validate_canonical_assertion_completeness(
        conn,
        session_id,
        generation_i64,
        watermarks.source_frontier(),
        control,
    )
    .await?;
    Ok(())
}

#[hotpath::measure(
    future = true,
    label = "session_temporal.projection.validate_assertions"
)]
pub(super) async fn validate_canonical_assertion_completeness(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &tracedecay_domain::SessionId,
    generation: i64,
    source_frontier: u64,
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    checkpoint_relation_rebuild_control(control)?;
    let mut rows = conn
        .query(
            "SELECT observation.observation_json, anchor.anchor_json
             FROM session_temporal_observation_effects AS effect
             JOIN observations AS observation
               ON observation.observation_id = effect.observation_id
             JOIN observation_retrieval_anchors AS binding
               ON binding.observation_id = observation.observation_id
             JOIN retrieval_anchors AS anchor ON anchor.anchor_id = binding.anchor_id
             WHERE effect.session_id = ?1
               AND effect.observation_sequence <= ?2
               AND effect.output_count > 0
             ORDER BY effect.observation_sequence",
            params![
                session_id.as_str(),
                frontier_i64(source_frontier, super::super::query::ACTIVATE_OPERATION,)?,
            ],
        )
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    record_assertion_validation_probe();
    let mut required = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?
    {
        checkpoint_relation_rebuild_control(control)?;
        let observation_json = row
            .get::<String>(0)
            .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
        let anchor_json = row
            .get::<String>(1)
            .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
        record_assertion_history_row(
            u64::try_from(observation_json.len().saturating_add(anchor_json.len()))
                .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?,
        );
        let observation: tracedecay_domain::DurableObservationV1 =
            serde_json::from_str(&observation_json)
                .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
        let Ok(envelope) = observation_envelope_from_payload(observation.payload()) else {
            continue;
        };
        if envelope.relations().session_id() != session_id {
            continue;
        }
        let anchor: RetrievalAnchorRecord = serde_json::from_str(&anchor_json)
            .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
        if anchor.owner() != observation.scope()
            || !anchor
                .source_observations()
                .contains(observation.observation_id())
        {
            return Err(storage_message(
                super::super::query::ACTIVATE_OPERATION,
                "canonical assertion lineage is not bound to its owning observation",
            ));
        }
        for lineage in anchor.source_anchors() {
            if let Some(kind) = assertion_kind_for_relation(lineage.relation()) {
                required.insert((
                    observation.observation_id().as_str().to_owned(),
                    anchor.anchor_id().as_str().to_owned(),
                    lineage.anchor_id().as_str().to_owned(),
                    kind.as_str().to_owned(),
                    observation
                        .receipt()
                        .receipt()
                        .receipt_id()
                        .as_str()
                        .to_owned(),
                ));
            }
        }
    }
    drop(rows);

    let mut actual = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT subject.source_observation_id,
                    assertion.subject_anchor_id, assertion.object_anchor_id,
                    assertion.assertion_kind,
                    json_extract(
                        assertion.evidence_json,
                        '$.sanitization_receipt.receipt_id'
                    )
             FROM session_assertions AS assertion
             JOIN (
                 SELECT DISTINCT session_id, generation,
                                 retrieval_anchor_id, source_observation_id
                 FROM session_occurrences
                 WHERE session_id = ?1 AND generation = ?2
             ) AS subject
               ON subject.session_id = assertion.session_id
              AND subject.generation = assertion.generation
              AND subject.retrieval_anchor_id = assertion.subject_anchor_id
             WHERE assertion.session_id = ?1 AND assertion.generation = ?2
               AND EXISTS (
                   SELECT 1
                   FROM session_occurrences AS object
                   WHERE object.session_id = assertion.session_id
                     AND object.generation = assertion.generation
                     AND object.retrieval_anchor_id = assertion.object_anchor_id
               )",
            params![session_id.as_str(), generation],
        )
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    record_assertion_validation_probe();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?
    {
        checkpoint_relation_rebuild_control(control)?;
        let receipt_id = row
            .get::<Option<String>>(4)
            .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
        if let Some(receipt_id) = receipt_id {
            actual.insert((
                row.get::<String>(0)
                    .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?,
                row.get::<String>(1)
                    .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?,
                row.get::<String>(2)
                    .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?,
                row.get::<String>(3)
                    .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?,
                receipt_id,
            ));
        }
    }
    if !required.is_subset(&actual) {
        return Err(storage_message(
            super::super::query::ACTIVATE_OPERATION,
            "candidate omits canonical typed assertion lineage through the frozen frontier",
        ));
    }
    Ok(())
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    encode_tagged_lowercase_hex("sha256:", &Sha256::digest(bytes))
}

#[hotpath::measure(future = true, label = "session_temporal.persist.observation_effect")]
pub async fn record_canonical_observation_effect(
    conn: &impl Executor,
    sequence: u64,
    observation: &tracedecay_domain::DurableObservationV1,
    effect: &ObservationProjection,
) -> ProjectionStoreResult<()> {
    let Ok(envelope) = observation_envelope_from_payload(observation.payload()) else {
        return Ok(());
    };
    let mut outputs = effect
        .messages()
        .map(|output| {
            Ok(json!({
                "anchor_id": output.provenance().retrieval_anchor_id().as_str(),
                "digest": output.output_digest()?.as_str(),
                "ordinal": output.output_ordinal(),
                "provider": output.message().provider,
                "message_id": output.message().message_id,
                "session_id": output.session().session_id,
            }))
        })
        .collect::<ProjectionStoreResult<Vec<_>>>()?;
    outputs.sort_unstable_by_key(ToString::to_string);
    let temporal_output_count = outputs.len();
    let effect_digest = digest_bytes(
        &serde_json::to_vec(&json!({
            "observation_id": observation.observation_id().as_str(),
            "output_count": temporal_output_count,
            "outputs": outputs,
            "session_id": envelope.relations().session_id().as_str(),
        }))
        .map_err(|_| {
            ProjectionStoreError::Contract(
                tracedecay_domain::ObservationContractError::CanonicalEncoding,
            )
        })?,
    );
    let sequence =
        i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
    let output_count = i64::try_from(temporal_output_count)
        .map_err(|_| ProjectionStoreError::SequenceOverflow(u64::MAX))?;
    let inserted = conn
        .execute(
            "INSERT INTO session_temporal_observation_effects (
            observation_id, observation_sequence, session_id, receipt_id,
            effect_digest, output_count, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch() * 1000000)
         ON CONFLICT(observation_id) DO NOTHING",
            params![
                observation.observation_id().as_str(),
                sequence,
                envelope.relations().session_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                effect_digest.as_str(),
                output_count,
            ],
        )
        .await
        .map_err(|error| ProjectionStoreError::Storage {
            operation: "record canonical temporal observation effect",
            source: Box::new(error),
        })?;
    // `ON CONFLICT(observation_id) DO NOTHING` reports one changed row when the
    // effect was newly written and zero when the primary key already held one.
    // A fresh insert wrote the tuple above inside this transaction, and the
    // table is insert-only (immutable update/delete triggers plus an authority
    // guard on insert), so reading it back could only echo these very
    // parameters. Only the conflict branch can hide a durable row that
    // disagrees with this derivation, so the read-back comparison — the actual
    // provenance contract for replayed observations — is confined to it.
    if inserted == 1 {
        return Ok(());
    }
    let mut rows = conn
        .query(
            "SELECT observation_sequence, session_id, receipt_id, effect_digest, output_count
             FROM session_temporal_observation_effects WHERE observation_id = ?1",
            params![observation.observation_id().as_str()],
        )
        .await
        .map_err(|error| ProjectionStoreError::Storage {
            operation: "verify canonical temporal observation effect",
            source: Box::new(error),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|error| ProjectionStoreError::Storage {
            operation: "verify canonical temporal observation effect",
            source: Box::new(error),
        })?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    let actual = (
        row.get::<i64>(0)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<String>(1)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<String>(2)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<String>(3)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<i64>(4)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
    );
    let expected = (
        sequence,
        envelope.relations().session_id().as_str().to_owned(),
        observation
            .receipt()
            .receipt()
            .receipt_id()
            .as_str()
            .to_owned(),
        effect_digest,
        output_count,
    );
    if actual == expected {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

pub(super) fn sorted_json<T: serde::Serialize>(values: &[T]) -> SessionStoreResult<Vec<String>> {
    let mut encoded = values
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    encoded.sort_unstable();
    Ok(encoded)
}

pub(super) fn canonical_batch_digest(
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<SessionTemporalDigestV1> {
    let encoded = serde_json::to_vec(&json!({
        "assertions": sorted_json(batch.assertions())?,
        "copies": sorted_json(batch.copies())?,
        "generation": batch.generation().value(),
        "occurrences": sorted_json(batch.occurrences())?,
        "projection_through": batch.projection_through(),
        "session_id": batch.session_id().as_str(),
        "source_through": batch.source_through(),
        "watermarks": encode_watermarks(batch.watermarks(), PERSIST_OPERATION)?,
    }))
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    SessionTemporalDigestV1::new(digest_bytes(&encoded))
}

pub(super) async fn read_projection_receipt(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    batch_digest: &str,
) -> SessionStoreResult<Option<SessionTemporalProjectionBatchReceiptV1>> {
    let mut rows = conn
        .query(
            "SELECT batch_digest, frozen_watermarks_json, source_through,
                    projection_through, committed_at
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2 AND batch_ordinal = ?3",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
                frontier_i64(batch.batch_ordinal(), PERSIST_OPERATION)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    else {
        let mut digest_rows = conn
            .query(
                "SELECT batch_ordinal FROM session_temporal_projection_receipts
                 WHERE session_id = ?1 AND generation = ?2 AND batch_digest = ?3",
                params![
                    batch.session_id().as_str(),
                    generation_i64(batch.generation(), PERSIST_OPERATION)?,
                    batch_digest,
                ],
            )
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        if digest_rows
            .next()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?
            .is_some()
        {
            return Err(storage_message(
                PERSIST_OPERATION,
                "projection batch digest is already bound to a different ordinal",
            ));
        }
        return Ok(None);
    };
    let actual_digest: String = row
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual_watermarks: String = row
        .get(1)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual_source: i64 = row
        .get(2)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual_projection: i64 = row
        .get(3)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let committed_at: i64 = row
        .get(4)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if actual_digest != batch_digest
        || actual_watermarks != encode_watermarks(batch.watermarks(), PERSIST_OPERATION)?
        || u64::try_from(actual_source).ok() != Some(batch.source_through())
        || u64::try_from(actual_projection).ok() != Some(batch.projection_through())
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "projection batch ordinal conflicts with its immutable receipt",
        ));
    }
    let batch_digest = SessionTemporalDigestV1::new(actual_digest)?;
    let existing = SessionTemporalProjectionBatchReceiptV1::applied(
        batch,
        batch_digest.clone(),
        batch.occurrences().len(),
        batch.copies().len(),
        batch.assertions().len(),
        tracedecay_domain::UtcMicros(committed_at),
    )?;
    Ok(Some(SessionTemporalProjectionBatchReceiptV1::exact_replay(
        batch,
        batch_digest,
        &existing,
        tracedecay_domain::UtcMicros(committed_at),
    )?))
}

pub(super) async fn require_contiguous_checkpoint(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT batch_ordinal, source_through, projection_through
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2
             ORDER BY batch_ordinal DESC LIMIT 1",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let previous = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    match previous {
        None if batch.batch_ordinal() == 0 => Ok(()),
        Some(row) => {
            let ordinal: i64 = row
                .get(0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let source: i64 = row
                .get(1)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let projection: i64 = row
                .get(2)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let expected = u64::try_from(ordinal)
                .map_err(|error| storage(PERSIST_OPERATION, error))?
                .saturating_add(1);
            if batch.batch_ordinal() != expected
                || u64::try_from(source)
                    .ok()
                    .is_none_or(|value| value > batch.source_through())
                || u64::try_from(projection)
                    .ok()
                    .is_none_or(|value| value > batch.projection_through())
            {
                return Err(storage_message(
                    PERSIST_OPERATION,
                    "projection batch checkpoint is not contiguous and monotonic",
                ));
            }
            Ok(())
        }
        None => Err(storage_message(
            PERSIST_OPERATION,
            "projection batch checkpoint must start at ordinal zero",
        )),
    }
}

pub(super) async fn digest_query_rows(
    conn: &impl crate::handle::SessionTemporalExec,
    sql: &str,
    batch: &SessionTemporalProjectionBatchV1,
    control: Option<&ExecutionControl>,
) -> SessionStoreResult<(usize, String)> {
    if let Some(control) = control {
        checkpoint_relation_rebuild_control(control)?;
    }
    record_coverage_query_probe();
    let mut rows = conn
        .query(
            sql,
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let mut digest = Sha256::new();
    let mut count = 0usize;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        if let Some(control) = control {
            checkpoint_relation_rebuild_control(control)?;
        }
        let value = row
            .get::<String>(0)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        record_coverage_row(
            u64::try_from(value.len()).map_err(|error| storage(PERSIST_OPERATION, error))?,
        );
        update_ordered_row_digest(&mut digest, count, value.as_bytes());
        count = count
            .checked_add(1)
            .ok_or_else(|| storage_message(PERSIST_OPERATION, "coverage row count overflow"))?;
    }
    Ok((
        count,
        encode_tagged_lowercase_hex("sha256:", &digest.finalize()),
    ))
}

fn update_ordered_row_digest(digest: &mut Sha256, prior_rows: usize, row: &[u8]) {
    if prior_rows > 0 {
        digest.update(b"\n");
    }
    digest.update(row);
}

#[hotpath::measure(future = true, label = "session_temporal.projection.coverage")]
pub(super) async fn projection_coverage(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    control: Option<&ExecutionControl>,
) -> SessionStoreResult<ProjectionCoverage> {
    let occurrences = digest_query_rows(
        conn,
        "SELECT json_array(occurrence_id, source_observation_id,
                source_provider, projection_output_ordinal, retrieval_anchor_id, thread_id,
                thread_grouping_json, turn_id, turn_grouping_json, message_id,
                agent_id, role, knowledge_at, valid_time_json, evidence_json,
                sanitized_content_digest, sanitized_content_bytes,
                snippet_text, index_text)
         FROM session_occurrences
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY occurrence_id",
        batch,
        control,
    )
    .await?;
    let dimensions = digest_query_rows(
        conn,
        "SELECT encoded FROM (
            SELECT 'agent:' || json_array(agent_id, agent_json, created_at) AS encoded
            FROM session_agents WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'thread:' || json_array(thread_id, grouping_provenance, created_at)
            FROM session_threads WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'turn:' || json_array(turn_id, ordinal, grouping_provenance, created_at)
            FROM session_turns WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'member:' || json_array(turn_id, occurrence_id, ordinal)
            FROM session_turn_members WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'derived:' || json_array(
                evidence_kind, evidence_id, retrieval_anchor_id, thread_id,
                first_occurrence_id, last_occurrence_id, algorithm_version,
                configuration_digest, member_count, member_digest, evidence_json
            )
            FROM session_derived_evidence
            WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'derived-member:' || json_array(
                evidence_kind, evidence_id, ordinal, occurrence_id, member_role
            )
            FROM session_derived_evidence_members
            WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'derived-anchor:' || json_array(
                anchor.anchor_id, anchor.anchor_json, anchor.owner_json,
                anchor.projection_generation
            )
            FROM session_derived_evidence AS evidence
            JOIN retrieval_anchors AS anchor
              ON anchor.anchor_id = evidence.retrieval_anchor_id
            WHERE evidence.session_id = ?1 AND evidence.generation = ?2
         ) ORDER BY encoded",
        batch,
        control,
    )
    .await?;
    if let Some(control) = control {
        checkpoint_relation_rebuild_control(control)?;
    }
    let cancellation = control
        .map(super::super::store::execution_control_graph_cancellation)
        .unwrap_or_else(|| Arc::new(NeverCancelled));
    let copies = reconstruct_logical_copy_relations(
        conn,
        batch.session_id(),
        batch.generation(),
        MAX_RECEIPT_COPY_ENTITIES,
        cancellation,
    )
    .await
    .and_then(|copies| copy_identity_coverage(&copies))?;
    if let Some(control) = control {
        checkpoint_relation_rebuild_control(control)?;
    }
    let assertions = digest_query_rows(
        conn,
        "SELECT json_array(assertion_id, assertion_kind, subject_anchor_id,
                object_anchor_id, knowledge_at, valid_time_json, evidence_json)
         FROM session_assertions
         WHERE session_id = ?1 AND generation = ?2 ORDER BY assertion_id",
        batch,
        control,
    )
    .await?;
    let supersession = digest_query_rows(
        conn,
        "SELECT json_array(superseded_assertion_id, superseding_assertion_id, created_at)
         FROM session_assertion_supersession
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY superseded_assertion_id, superseding_assertion_id",
        batch,
        control,
    )
    .await?;
    let current = digest_query_rows(
        conn,
        "SELECT json_array(entity_kind, entity_id, current_assertion_id,
                current_occurrence_id, coverage_json)
         FROM session_current_entities
         WHERE session_id = ?1 AND generation = ?2 ORDER BY entity_kind, entity_id",
        batch,
        control,
    )
    .await?;
    let fts = digest_query_rows(
        conn,
        "SELECT json_array(occurrence.occurrence_id, fts.index_text, fts.snippet_text)
         FROM session_occurrences AS occurrence
         JOIN session_occurrences_fts AS fts ON fts.rowid = occurrence.rowid
         WHERE occurrence.session_id = ?1 AND occurrence.generation = ?2
         ORDER BY occurrence.occurrence_id",
        batch,
        control,
    )
    .await?;
    Ok(ProjectionCoverage {
        occurrences,
        dimensions,
        copies,
        assertions,
        supersession,
        current,
        fts,
    })
}

#[inline(always)]
fn record_assertion_validation_probe() {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.activation.assertion_query_probes").inc(1_u64);
}

#[inline(always)]
fn record_assertion_history_row(bytes: u64) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("session_temporal.activation.history_rows").inc(1_u64);
        hotpath::gauge!("session_temporal.activation.history_row_payload_bytes").inc(bytes);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = bytes;
}

#[inline(always)]
fn record_coverage_query_probe() {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.coverage.query_probes").inc(1_u64);
}

#[inline(always)]
fn record_coverage_row(bytes: u64) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("session_temporal.coverage.rows").inc(1_u64);
        hotpath::gauge!("session_temporal.coverage.row_payload_bytes").inc(bytes);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = bytes;
}

fn copy_identity_coverage(copies: &[LogicalCopyRelation]) -> SessionStoreResult<(usize, String)> {
    let encoded = sorted_json(copies)?;
    Ok((encoded.len(), digest_bytes(encoded.join("\n").as_bytes())))
}

pub(super) async fn insert_projection_receipt(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    batch_digest: &str,
    coverage: &ProjectionCoverage,
    committed_at: i64,
    baseline: ProjectionProgressBaseline,
) -> SessionStoreResult<()> {
    let (committed_item_count, committed_copy_count) =
        projection_progress_counts(conn, batch, baseline).await?;
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest,
            frozen_watermarks_json, source_through, projection_through,
            batch_item_count, committed_item_count, committed_copy_count,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22,
            ?23, ?24, ?25
         )",
        params![
            batch.session_id().as_str(),
            generation_i64(batch.generation(), PERSIST_OPERATION)?,
            frontier_i64(batch.batch_ordinal(), PERSIST_OPERATION)?,
            batch_digest,
            encode_watermarks(batch.watermarks(), PERSIST_OPERATION)?,
            frontier_i64(batch.source_through(), PERSIST_OPERATION)?,
            frontier_i64(batch.projection_through(), PERSIST_OPERATION)?,
            i64::try_from(batch.item_count()).map_err(|error| storage(PERSIST_OPERATION, error))?,
            i64::try_from(committed_item_count)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            i64::try_from(committed_copy_count)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            i64::try_from(coverage.occurrences.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.occurrences.1.as_str(),
            i64::try_from(coverage.dimensions.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.dimensions.1.as_str(),
            i64::try_from(coverage.copies.0).map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.copies.1.as_str(),
            i64::try_from(coverage.assertions.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.assertions.1.as_str(),
            i64::try_from(coverage.supersession.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.supersession.1.as_str(),
            i64::try_from(coverage.current.0).map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.current.1.as_str(),
            i64::try_from(coverage.fts.0).map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.fts.1.as_str(),
            committed_at,
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}

async fn projection_progress_counts(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    baseline: ProjectionProgressBaseline,
) -> SessionStoreResult<(usize, usize)> {
    let prior = if batch.batch_ordinal() == 0
        && matches!(baseline, ProjectionProgressBaseline::SeededFromActive)
    {
        (batch.watermarks().active_generation(), None)
    } else if batch.batch_ordinal() > 0 {
        (
            batch.generation(),
            Some(batch.batch_ordinal().saturating_sub(1)),
        )
    } else {
        return Ok((batch.item_count(), batch.copies().len()));
    };
    let (generation, ordinal) = prior;
    let mut rows = conn
        .query(
            "SELECT committed_item_count, committed_copy_count
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2
               AND (?3 IS NULL OR batch_ordinal = ?3)
             ORDER BY batch_ordinal DESC LIMIT 1",
            params![
                batch.session_id().as_str(),
                generation_i64(generation, PERSIST_OPERATION)?,
                ordinal
                    .map(|value| frontier_i64(value, PERSIST_OPERATION))
                    .transpose()?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let (prior_items, prior_copies) = match rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        Some(row) => (
            usize::try_from(
                row.get::<i64>(0)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?,
            )
            .map_err(|error| storage(PERSIST_OPERATION, error))?,
            usize::try_from(
                row.get::<i64>(1)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?,
            )
            .map_err(|error| storage(PERSIST_OPERATION, error))?,
        ),
        None => (0, 0),
    };
    Ok((
        prior_items
            .checked_add(batch.item_count())
            .ok_or_else(|| storage_message(PERSIST_OPERATION, "committed item count overflow"))?,
        prior_copies
            .checked_add(batch.copies().len())
            .ok_or_else(|| storage_message(PERSIST_OPERATION, "committed copy count overflow"))?,
    ))
}

#[derive(PartialEq, Eq)]
pub(super) struct ProjectionCoverage {
    occurrences: (usize, String),
    dimensions: (usize, String),
    copies: (usize, String),
    assertions: (usize, String),
    supersession: (usize, String),
    current: (usize, String),
    fts: (usize, String),
}

pub(super) fn empty_projection_coverage() -> ProjectionCoverage {
    let digest = digest_bytes(&[]);
    ProjectionCoverage {
        occurrences: (0, digest.clone()),
        dimensions: (0, digest.clone()),
        copies: (0, digest.clone()),
        assertions: (0, digest.clone()),
        supersession: (0, digest.clone()),
        current: (0, digest.clone()),
        fts: (0, digest),
    }
}

#[cfg(test)]
mod digest_tests {
    use super::*;

    #[test]
    fn streaming_row_digest_preserves_the_canonical_joined_bytes() {
        let rows = [b"alpha".as_slice(), b"".as_slice(), b"omega".as_slice()];
        let mut streaming = Sha256::new();
        for (index, row) in rows.into_iter().enumerate() {
            update_ordered_row_digest(&mut streaming, index, row);
        }
        assert_eq!(
            encode_tagged_lowercase_hex("sha256:", &streaming.finalize()),
            digest_bytes(b"alpha\n\nomega")
        );
    }
}
