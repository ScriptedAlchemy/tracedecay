use std::collections::BTreeSet;

use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_domain::RetrievalAnchorRecord;
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_runtime_core::db::engine::{Executor, params};
use tracedecay_store::{
    ObservationProjection, ProjectionStoreError, ProjectionStoreResult, SessionStoreResult,
    SessionTemporalDigestV1, SessionTemporalProjectionBatchReceiptV1,
    SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::execution::ExecutionControl;

use super::super::query::{
    PERSIST_OPERATION, encode_watermarks, frontier_i64, generation_i64, storage, storage_message,
};
use super::super::rebuild::checkpoint_relation_rebuild_control;
use super::super::relation_projection::{IntroducedCopies, introduced_logical_copies};
use super::super::relations::{LogicalCopyRelation, SessionRelationProjection};
use super::persist::*;
use crate::sql::SHARED_GENERATION_TABLES;

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
        occurrences: RowMultisetDigest::decode(count(2)?, &digest(3)?)?,
        dimensions: RowMultisetDigest::decode(count(4)?, &digest(5)?)?,
        copies: RowMultisetDigest::decode(count(6)?, &digest(7)?)?,
        assertions: RowMultisetDigest::decode(count(8)?, &digest(9)?)?,
        supersession: RowMultisetDigest::decode(count(10)?, &digest(11)?)?,
        current: RowMultisetDigest::decode(count(12)?, &digest(13)?)?,
        fts: RowMultisetDigest::decode(count(14)?, &digest(15)?)?,
    };
    drop(rows);
    let actual = candidate_projection_coverage(conn, session_id, generation, control).await?;
    if actual != expected {
        return Err(storage_message(
            super::super::query::ACTIVATE_OPERATION,
            "candidate projection rows do not match the immutable final receipt",
        ));
    }
    if copy_coverage(&relation_projection.logical_copies)? != expected.copies {
        return Err(storage_message(
            super::super::query::ACTIVATE_OPERATION,
            "native relation graph copies do not match the immutable final receipt",
        ));
    }
    validate_canonical_assertion_completeness(
        conn,
        session_id,
        generation_i64,
        base_source_frontier(conn, session_id, generation_i64).await?,
        watermarks.source_frontier(),
        control,
    )
    .await?;
    Ok(())
}

/// Source frontier a candidate extends: the frontier the latest activated
/// generation below it projected and proved, or zero for a session's first
/// rows. A bootstrap generation activates without rows and summary
/// publication without projection, so neither moves this frontier.
pub(crate) async fn base_source_frontier(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &tracedecay_domain::SessionId,
    generation: i64,
) -> SessionStoreResult<u64> {
    let mut rows = conn
        .query(
            "SELECT receipt.source_through
             FROM session_temporal_projection_receipts AS receipt
             JOIN session_temporal_generations AS settled
               ON settled.session_id = receipt.session_id
              AND settled.generation = receipt.generation
             WHERE receipt.session_id = ?1 AND receipt.generation < ?2
               AND settled.state IN ('active', 'superseded')
             ORDER BY receipt.generation DESC, receipt.batch_ordinal DESC
             LIMIT 1",
            params![session_id.as_str(), generation],
        )
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?;
    match rows
        .next()
        .await
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?
    {
        Some(row) => u64::try_from(
            row.get::<i64>(0)
                .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error))?,
        )
        .map_err(|error| storage(super::super::query::ACTIVATE_OPERATION, error)),
        None => Ok(0),
    }
}

/// The base generation proved its own lineage at activation, so a candidate
/// proves only the effects past the base frontier against the assertions
/// whose subjects it introduced.
pub(super) async fn validate_canonical_assertion_completeness(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &tracedecay_domain::SessionId,
    generation: i64,
    base_frontier: u64,
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
               AND effect.observation_sequence > ?3
               AND effect.observation_sequence <= ?2
               AND effect.output_count > 0
             ORDER BY effect.observation_sequence",
            params![
                session_id.as_str(),
                frontier_i64(source_frontier, super::super::query::ACTIVATE_OPERATION,)?,
                frontier_i64(base_frontier, super::super::query::ACTIVATE_OPERATION,)?,
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
                 SELECT DISTINCT retrieval_anchor_id, source_observation_id
                 FROM session_occurrences
                 WHERE session_id = ?1 AND generation = ?2
             ) AS subject
               ON subject.retrieval_anchor_id = assertion.subject_anchor_id
             WHERE assertion.session_id = ?1 AND assertion.generation <= ?2
               AND EXISTS (
                   SELECT 1
                   FROM session_occurrences AS object
                   WHERE object.session_id = ?1
                     AND object.generation <= ?2
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
    // disagrees with this derivation, so the read-back comparison, the actual
    // provenance contract for replayed observations, is confined to it.
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

fn sorted_json<T: serde::Serialize>(values: &[T]) -> SessionStoreResult<Vec<String>> {
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

/// Order-independent digest of a row multiset: each row's canonical
/// encoding hashes to a 256-bit integer and the digest is their sum modulo
/// 2^256. A candidate's coverage is therefore its base's coverage plus the
/// rows it added and minus the versions it superseded, and it still equals
/// a digest recomputed over every row the generation reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowMultisetDigest {
    count: usize,
    /// Big-endian 64-bit limbs.
    sum: [u64; 4],
}

const ROW_MULTISET_TAG: &str = "sha256-sum:";

impl RowMultisetDigest {
    fn row_limbs(row: &[u8]) -> [u64; 4] {
        let hash: [u8; 32] = Sha256::digest(row).into();
        let mut limbs = [0_u64; 4];
        for (limb, chunk) in limbs.iter_mut().zip(hash.chunks_exact(8)) {
            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(chunk);
            *limb = u64::from_be_bytes(bytes);
        }
        limbs
    }

    pub(crate) fn add(&mut self, row: &[u8]) -> SessionStoreResult<()> {
        let limbs = Self::row_limbs(row);
        let mut carry = false;
        for index in (0..4).rev() {
            let (partial, first) = self.sum[index].overflowing_add(limbs[index]);
            let (value, second) = partial.overflowing_add(u64::from(carry));
            self.sum[index] = value;
            carry = first || second;
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| storage_message(PERSIST_OPERATION, "coverage row count overflow"))?;
        Ok(())
    }

    pub(crate) fn remove(&mut self, row: &[u8]) -> SessionStoreResult<()> {
        let limbs = Self::row_limbs(row);
        let mut borrow = false;
        for index in (0..4).rev() {
            let (partial, first) = self.sum[index].overflowing_sub(limbs[index]);
            let (value, second) = partial.overflowing_sub(u64::from(borrow));
            self.sum[index] = value;
            borrow = first || second;
        }
        self.count = self.count.checked_sub(1).ok_or_else(|| {
            storage_message(PERSIST_OPERATION, "coverage removes a row it never held")
        })?;
        Ok(())
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn digest(&self) -> String {
        let mut bytes = [0_u8; 32];
        for (chunk, limb) in bytes.chunks_exact_mut(8).zip(self.sum) {
            chunk.copy_from_slice(&limb.to_be_bytes());
        }
        encode_tagged_lowercase_hex(ROW_MULTISET_TAG, &bytes)
    }

    fn decode(count: usize, digest: &str) -> SessionStoreResult<Self> {
        let invalid = || storage_message(PERSIST_OPERATION, "coverage digest is malformed");
        let hex = digest.strip_prefix(ROW_MULTISET_TAG).ok_or_else(invalid)?;
        if hex.len() != 64 {
            return Err(invalid());
        }
        let mut sum = [0_u64; 4];
        for (limb, index) in sum.iter_mut().zip((0..64).step_by(16)) {
            *limb = u64::from_str_radix(hex.get(index..index + 16).ok_or_else(invalid)?, 16)
                .map_err(|_| invalid())?;
        }
        let decoded = Self { count, sum };
        if decoded.digest() != digest {
            return Err(invalid());
        }
        Ok(decoded)
    }
}

/// Which rows of one shared table a coverage query hashes.
#[derive(Clone, Copy)]
enum CoverageRows {
    /// Every row generation `?2` reads.
    Visible,
    /// Rows generation `?2` introduced.
    Introduced,
    /// Older versions of keys generation `?2` re-versioned.
    Superseded,
}

/// One shared table's contribution to a coverage component: `encoding` is
/// the canonical row text over alias `row` (joined through `from`).
struct CoverageSource {
    table: &'static str,
    from: &'static str,
    encoding: &'static str,
}

const OCCURRENCE_ENCODING: &str = "json_array(row.occurrence_id, row.source_observation_id,
    row.source_sequence, row.source_provider, row.projection_output_ordinal,
    row.retrieval_anchor_id, row.thread_id, row.thread_grouping_json, row.turn_id,
    row.turn_grouping_json, row.message_id, row.agent_id, row.parent_message_id,
    row.parent_agent_id, row.parent_session_id, row.copied_from_anchor_ids_json, row.role,
    row.knowledge_at, row.valid_time_json, row.evidence_json, row.sanitized_content_digest,
    row.sanitized_content_bytes, row.snippet_text, row.index_text)";

const OCCURRENCE_SOURCES: &[CoverageSource] = &[CoverageSource {
    table: "session_occurrences",
    from: "session_occurrences AS row",
    encoding: OCCURRENCE_ENCODING,
}];

const DIMENSION_SOURCES: &[CoverageSource] = &[
    CoverageSource {
        table: "session_agents",
        from: "session_agents AS row",
        encoding: "'agent:' || json_array(row.agent_id, row.agent_json, row.created_at)",
    },
    CoverageSource {
        table: "session_threads",
        from: "session_threads AS row",
        encoding: "'thread:' || json_array(row.thread_id, row.grouping_provenance, row.created_at)",
    },
    CoverageSource {
        table: "session_turns",
        from: "session_turns AS row",
        encoding: "'turn:' || json_array(row.turn_id, row.ordinal, row.grouping_provenance, row.created_at)",
    },
    CoverageSource {
        table: "session_turn_members",
        from: "session_turn_members AS row",
        encoding: "'member:' || json_array(row.turn_id, row.occurrence_id, row.ordinal)",
    },
    CoverageSource {
        table: "session_derived_evidence",
        from: "session_derived_evidence AS row",
        encoding: "'derived:' || json_array(
            row.evidence_kind, row.evidence_id, row.retrieval_anchor_id, row.thread_id,
            row.first_occurrence_id, row.last_occurrence_id, row.algorithm_version,
            row.configuration_digest, row.member_count, row.member_digest, row.evidence_json
        )",
    },
    CoverageSource {
        table: "session_derived_evidence_members",
        from: "session_derived_evidence_members AS row",
        encoding: "'derived-member:' || json_array(
            row.evidence_kind, row.first_occurrence_id, row.ordinal, row.occurrence_id,
            row.member_role
        )",
    },
    CoverageSource {
        table: "session_derived_evidence",
        from: "session_derived_evidence AS row
               JOIN retrieval_anchors AS anchor ON anchor.anchor_id = row.retrieval_anchor_id",
        encoding: "'derived-anchor:' || json_array(
            anchor.anchor_id, anchor.anchor_json, anchor.owner_json, anchor.projection_generation
        )",
    },
];

const ASSERTION_SOURCES: &[CoverageSource] = &[CoverageSource {
    table: "session_assertions",
    from: "session_assertions AS row",
    encoding: "json_array(row.assertion_id, row.assertion_kind, row.subject_anchor_id,
        row.object_anchor_id, row.knowledge_at, row.valid_time_json, row.evidence_json)",
}];

const SUPERSESSION_SOURCES: &[CoverageSource] = &[CoverageSource {
    table: "session_assertion_supersession",
    from: "session_assertion_supersession AS row",
    encoding: "json_array(row.superseded_assertion_id, row.superseding_assertion_id,
        row.created_at)",
}];

const CURRENT_SOURCES: &[CoverageSource] = &[CoverageSource {
    table: "session_current_entities",
    from: "session_current_entities AS row",
    encoding: "json_array(row.entity_kind, row.entity_id, row.current_assertion_id,
        row.current_occurrence_id, row.coverage_json)",
}];

const FTS_SOURCES: &[CoverageSource] = &[CoverageSource {
    table: "session_occurrences",
    from: "session_occurrences AS row
           CROSS JOIN session_occurrences_fts AS fts ON fts.rowid = row.rowid",
    encoding: "json_array(row.occurrence_id, fts.index_text)",
}];

/// One exact-SQL page of coverage rows; `snippet_text` and `index_text` are
/// the row bulk, so pages are row-count-small and keyed by rowid.
const COVERAGE_DIGEST_PAGE_ROWS: i64 = 32;

/// Pages walk `(generation, rowid)` of the driving rows through the
/// table's `(session_id, generation)` index, so a page never scans rows the
/// scope excludes.
fn coverage_sql(source: &CoverageSource, rows: CoverageRows) -> SessionStoreResult<Option<String>> {
    let table = SHARED_GENERATION_TABLES
        .iter()
        .find(|table| table.name == source.table)
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "coverage source is not a generation-shared table",
            )
        })?;
    let key_match = |alias: &str| {
        table
            .key
            .split(", ")
            .map(|column| format!("{alias}.{column} = row.{column}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let (driver, from, scope) = match rows {
        // A candidate reads both versions of a key it re-versioned until it
        // activates; the newest version at or below `?2` is the visible one.
        CoverageRows::Visible => (
            "row",
            source.from.to_owned(),
            if table.versioned {
                format!(
                    "row.generation <= ?2 AND NOT EXISTS (
                         SELECT 1 FROM {name} AS newer
                         WHERE newer.session_id = ?1 AND {matches}
                           AND newer.generation > row.generation AND newer.generation <= ?2
                     )",
                    name = table.name,
                    matches = key_match("newer"),
                )
            } else {
                "row.generation <= ?2".to_owned()
            },
        ),
        CoverageRows::Introduced => (
            "row",
            source.from.to_owned(),
            "row.generation = ?2".to_owned(),
        ),
        // Rows of an append-only table are never re-versioned.
        CoverageRows::Superseded if !table.versioned => return Ok(None),
        // Each key the candidate re-versioned has one older visible version:
        // activation deletes a version once its successor activates.
        CoverageRows::Superseded => (
            "successor",
            format!("{} AS successor CROSS JOIN {}", table.name, source.from),
            format!(
                "successor.session_id = ?1 AND successor.generation = ?2
                 AND row.generation < ?2 AND {matches}",
                matches = key_match("successor"),
            ),
        ),
    };
    Ok(Some(format!(
        "SELECT {encoding}, {driver}.generation, {driver}.rowid FROM {from}
         WHERE row.session_id = ?1 AND {scope}
           AND ({driver}.generation, {driver}.rowid) > (?3, ?4)
         ORDER BY {driver}.generation, {driver}.rowid LIMIT ?5",
        encoding = source.encoding,
    )))
}

async fn fold_coverage_rows(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &str,
    generation: i64,
    sources: &[CoverageSource],
    rows: CoverageRows,
    digest: &mut RowMultisetDigest,
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    for source in sources {
        let Some(sql) = coverage_sql(source, rows)? else {
            continue;
        };
        let mut after = (0_i64, 0_i64);
        loop {
            checkpoint_relation_rebuild_control(control)?;
            record_coverage_query_probe();
            let mut page = conn
                .query(
                    &sql,
                    params![
                        session_id,
                        generation,
                        after.0,
                        after.1,
                        COVERAGE_DIGEST_PAGE_ROWS
                    ],
                )
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let mut page_rows = 0_i64;
            while let Some(row) = page
                .next()
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?
            {
                let encoded = row
                    .get::<String>(0)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?;
                after = (
                    row.get::<i64>(1)
                        .map_err(|error| storage(PERSIST_OPERATION, error))?,
                    row.get::<i64>(2)
                        .map_err(|error| storage(PERSIST_OPERATION, error))?,
                );
                record_coverage_row(
                    u64::try_from(encoded.len())
                        .map_err(|error| storage(PERSIST_OPERATION, error))?,
                );
                match rows {
                    CoverageRows::Superseded => digest.remove(encoded.as_bytes())?,
                    CoverageRows::Visible | CoverageRows::Introduced => {
                        digest.add(encoded.as_bytes())?;
                    }
                }
                page_rows += 1;
            }
            if page_rows < COVERAGE_DIGEST_PAGE_ROWS {
                break;
            }
        }
    }
    Ok(())
}

fn coverage_components() -> [&'static [CoverageSource]; 6] {
    [
        OCCURRENCE_SOURCES,
        DIMENSION_SOURCES,
        ASSERTION_SOURCES,
        SUPERSESSION_SOURCES,
        CURRENT_SOURCES,
        FTS_SOURCES,
    ]
}

fn coverage_component_mut(
    coverage: &mut ProjectionCoverage,
    index: usize,
) -> &mut RowMultisetDigest {
    match index {
        0 => &mut coverage.occurrences,
        1 => &mut coverage.dimensions,
        2 => &mut coverage.assertions,
        3 => &mut coverage.supersession,
        4 => &mut coverage.current,
        _ => &mut coverage.fts,
    }
}

/// Coverage of a candidate generation: the base generation's final coverage
/// plus the rows the candidate introduced, minus the row versions it
/// superseded, with the copy component extended by the candidate's copies.
/// Reads only the candidate's own rows and their superseded versions.
#[hotpath::measure(future = true, label = "session_temporal.projection.coverage")]
pub(crate) async fn candidate_projection_coverage(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &tracedecay_domain::SessionId,
    generation: tracedecay_domain::SessionProjectionGenerationV1,
    control: &ExecutionControl,
) -> SessionStoreResult<ProjectionCoverage> {
    let cancellation = super::super::store::execution_control_graph_cancellation(control);
    let copies = match introduced_logical_copies(conn, session_id, generation, cancellation).await?
    {
        IntroducedCopies::Extend(copies) => copies,
        // An introduced occurrence sorted before the base's last instant, so
        // the settled relations were reconstructed; recompute every
        // component from the rows the candidate reads with them.
        IntroducedCopies::Reconstructed(copies) => {
            return full_projection_coverage(conn, session_id, generation, &copies, control).await;
        }
    };
    let generation_value = generation_i64(generation, PERSIST_OPERATION)?;
    let mut coverage = base_projection_coverage(conn, session_id, generation_value).await?;
    for (index, sources) in coverage_components().into_iter().enumerate() {
        let digest = coverage_component_mut(&mut coverage, index);
        for rows in [CoverageRows::Introduced, CoverageRows::Superseded] {
            fold_coverage_rows(
                conn,
                session_id.as_str(),
                generation_value,
                sources,
                rows,
                digest,
                control,
            )
            .await?;
        }
    }
    for copy in &copies {
        coverage.copies.add(&copy_encoding(copy)?)?;
    }
    Ok(coverage)
}

/// Coverage recomputed from every row generation `generation` reads: the
/// verifier the incremental receipt must equal byte for byte.
#[hotpath::measure(future = true, label = "session_temporal.projection.full_coverage")]
pub(crate) async fn full_projection_coverage(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &tracedecay_domain::SessionId,
    generation: tracedecay_domain::SessionProjectionGenerationV1,
    copies: &[LogicalCopyRelation],
    control: &ExecutionControl,
) -> SessionStoreResult<ProjectionCoverage> {
    let generation_value = generation_i64(generation, PERSIST_OPERATION)?;
    let mut coverage = empty_projection_coverage();
    for (index, sources) in coverage_components().into_iter().enumerate() {
        fold_coverage_rows(
            conn,
            session_id.as_str(),
            generation_value,
            sources,
            CoverageRows::Visible,
            coverage_component_mut(&mut coverage, index),
            control,
        )
        .await?;
    }
    coverage.copies = copy_coverage(copies)?;
    Ok(coverage)
}

/// Final coverage of the latest activated generation at or below
/// `generation` that projected rows. Summary publication activates
/// generations without projection receipts, and they read the same rows as
/// the projected generation below them.
pub(crate) async fn base_projection_coverage(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &tracedecay_domain::SessionId,
    generation: i64,
) -> SessionStoreResult<ProjectionCoverage> {
    let mut rows = conn
        .query(
            "SELECT receipt.occurrence_count, receipt.occurrence_digest,
                    receipt.dimension_count, receipt.dimension_digest,
                    receipt.copy_count, receipt.copy_digest,
                    receipt.assertion_count, receipt.assertion_digest,
                    receipt.supersession_count, receipt.supersession_digest,
                    receipt.current_count, receipt.current_digest,
                    receipt.fts_count, receipt.fts_digest
             FROM session_temporal_projection_receipts AS receipt
             JOIN session_temporal_generations AS settled
               ON settled.session_id = receipt.session_id
              AND settled.generation = receipt.generation
             WHERE receipt.session_id = ?1 AND receipt.generation < ?2
               AND settled.state IN ('active', 'superseded')
             ORDER BY receipt.generation DESC, receipt.batch_ordinal DESC
             LIMIT 1",
            params![session_id.as_str(), generation],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    else {
        return Ok(empty_projection_coverage());
    };
    let component = |index: i32| -> SessionStoreResult<RowMultisetDigest> {
        let count = usize::try_from(
            row.get::<i64>(index)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
        )
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let digest = row
            .get::<String>(index + 1)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        RowMultisetDigest::decode(count, &digest)
    };
    Ok(ProjectionCoverage {
        occurrences: component(0)?,
        dimensions: component(2)?,
        copies: component(4)?,
        assertions: component(6)?,
        supersession: component(8)?,
        current: component(10)?,
        fts: component(12)?,
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

fn copy_encoding(copy: &LogicalCopyRelation) -> SessionStoreResult<Vec<u8>> {
    serde_json::to_vec(copy).map_err(|error| storage(PERSIST_OPERATION, error))
}

pub(crate) fn copy_coverage(
    copies: &[LogicalCopyRelation],
) -> SessionStoreResult<RowMultisetDigest> {
    let mut digest = RowMultisetDigest::default();
    for copy in copies {
        digest.add(&copy_encoding(copy)?)?;
    }
    Ok(digest)
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
            i64::try_from(coverage.occurrences.count())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.occurrences.digest(),
            i64::try_from(coverage.dimensions.count())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.dimensions.digest(),
            i64::try_from(coverage.copies.count())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.copies.digest(),
            i64::try_from(coverage.assertions.count())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.assertions.digest(),
            i64::try_from(coverage.supersession.count())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.supersession.digest(),
            i64::try_from(coverage.current.count())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.current.digest(),
            i64::try_from(coverage.fts.count())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.fts.digest(),
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
    let seeded_from_active = batch.batch_ordinal() == 0
        && matches!(baseline, ProjectionProgressBaseline::SeededFromActive);
    let prior = if seeded_from_active {
        (batch.watermarks().active_generation(), None)
    } else if batch.batch_ordinal() > 0 {
        (
            batch.generation(),
            Some(batch.batch_ordinal().saturating_sub(1)),
        )
    } else {
        return Ok((batch.item_count(), batch.copies().len()));
    };
    let (prior_items, prior_copies) = match prior {
        (generation, Some(ordinal)) => {
            let mut rows = conn
                .query(
                    "SELECT committed_item_count, committed_copy_count
                     FROM session_temporal_projection_receipts
                     WHERE session_id = ?1 AND generation = ?2 AND batch_ordinal = ?3",
                    params![
                        batch.session_id().as_str(),
                        generation_i64(generation, PERSIST_OPERATION)?,
                        frontier_i64(ordinal, PERSIST_OPERATION)?,
                    ],
                )
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let row = rows
                .next()
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?
                .ok_or_else(|| {
                    storage_message(
                        PERSIST_OPERATION,
                        "prior projection batch receipt is unavailable",
                    )
                })?;
            let count = |index| -> SessionStoreResult<usize> {
                usize::try_from(
                    row.get::<i64>(index)
                        .map_err(|error| storage(PERSIST_OPERATION, error))?,
                )
                .map_err(|error| storage(PERSIST_OPERATION, error))
            };
            (count(0)?, count(1)?)
        }
        (_, None) => {
            let base = base_projection_coverage(
                conn,
                batch.session_id(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
            )
            .await?;
            (base.record_count(), base.copies.count())
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionCoverage {
    occurrences: RowMultisetDigest,
    dimensions: RowMultisetDigest,
    copies: RowMultisetDigest,
    assertions: RowMultisetDigest,
    supersession: RowMultisetDigest,
    current: RowMultisetDigest,
    fts: RowMultisetDigest,
}

impl ProjectionCoverage {
    /// Occurrences, assertions, and copies: the records a generation's
    /// refresh progress counts.
    pub(crate) fn record_count(&self) -> usize {
        self.occurrences
            .count()
            .saturating_add(self.assertions.count())
            .saturating_add(self.copies.count())
    }
}

pub(super) fn empty_projection_coverage() -> ProjectionCoverage {
    ProjectionCoverage {
        occurrences: RowMultisetDigest::default(),
        dimensions: RowMultisetDigest::default(),
        copies: RowMultisetDigest::default(),
        assertions: RowMultisetDigest::default(),
        supersession: RowMultisetDigest::default(),
        current: RowMultisetDigest::default(),
        fts: RowMultisetDigest::default(),
    }
}

#[cfg(test)]
mod digest_tests {
    use super::*;

    #[test]
    fn row_multiset_digest_is_order_independent_and_reversible() {
        let rows = [b"alpha".as_slice(), b"".as_slice(), b"omega".as_slice()];
        let mut forward = RowMultisetDigest::default();
        for row in rows {
            forward.add(row).unwrap();
        }
        let mut reversed = RowMultisetDigest::default();
        for row in rows.into_iter().rev() {
            reversed.add(row).unwrap();
        }
        assert_eq!(forward, reversed);
        assert_eq!(forward.count(), 3);

        let mut extended = forward;
        extended.add(b"late").unwrap();
        extended.remove(b"alpha").unwrap();
        let mut rebuilt = RowMultisetDigest::default();
        for row in [b"".as_slice(), b"omega".as_slice(), b"late".as_slice()] {
            rebuilt.add(row).unwrap();
        }
        assert_eq!(extended, rebuilt);
        assert_ne!(extended.digest(), forward.digest());
        assert_eq!(
            RowMultisetDigest::decode(extended.count(), &extended.digest()).unwrap(),
            extended
        );
        assert_eq!(
            RowMultisetDigest::default().digest(),
            format!("{ROW_MULTISET_TAG}{}", "0".repeat(64))
        );
        assert!(RowMultisetDigest::default().remove(b"alpha").is_err());
    }
}
