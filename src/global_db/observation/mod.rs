mod codec;
mod persist;
mod provenance_backfill;
mod schema;

#[cfg(test)]
pub(super) use schema::backfill_observation_retrieval_anchors;
pub(super) use schema::ensure_observation_schema;

use libsql::{Connection, params};
use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationScopeV1, ProjectionGenerationId, RetrievalAnchorId,
    RetrievalAnchorRecordV2, VectorWatermark,
};
use tracedecay_store::{
    ObservationCommitReceipt, ObservationProjectionStatus, ObservationReplayRequest,
    ObservationStoreError, ObservationStoreResult, ObservedEvidenceAnchorResolution,
    SESSION_MESSAGE_PROJECTOR_VERSION, StoredObservation,
};

use super::GlobalDb;
use codec::{
    decode, decode_repository_provenance_attachment, decode_sequence, storage, storage_message,
};
use persist::read_by_observation_id;

async fn read_observation_id_for_retrieval_anchor(
    conn: &Connection,
    anchor_id: &RetrievalAnchorId,
) -> ObservationStoreResult<Option<CanonicalObservationIdV1>> {
    let mut rows = conn
        .query(
            "SELECT observation_id FROM observation_retrieval_anchors
             WHERE anchor_id = ?1
             UNION
             SELECT observation_id FROM observation_repository_provenance
             WHERE retrieval_anchor_id = ?1",
            params![anchor_id.as_str()],
        )
        .await
        .map_err(|error| storage("read retrieval anchor observation binding", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read retrieval anchor observation binding", error))?
    else {
        return Ok(None);
    };
    let observation_id = row
        .get::<String>(0)
        .map_err(|error| storage("read retrieval anchor observation binding", error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage("read retrieval anchor observation binding", error))?
        .is_some()
    {
        return Err(ObservationStoreError::RetrievalAnchorCollision);
    }
    CanonicalObservationIdV1::new(observation_id)
        .map(Some)
        .map_err(ObservationStoreError::Contract)
}

/// Shared owner-bound anchor lookup for the record and typed-report
/// resolution paths. Both paths must never diverge in how they enforce the
/// retained record's identity, owner, and projection generation.
async fn resolve_owner_bound_anchor_record(
    conn: &Connection,
    owner: &ObservationScopeV1,
    anchor_id: &RetrievalAnchorId,
) -> ObservationStoreResult<Option<RetrievalAnchorRecordV2>> {
    let Some(observation_id) = read_observation_id_for_retrieval_anchor(conn, anchor_id).await?
    else {
        return Ok(None);
    };
    let receipt = read_by_observation_id(conn, &observation_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                "resolve evidence anchor",
                "retrieval anchor binding has no canonical observation",
            )
        })?;
    let record = if receipt.retrieval_anchor().anchor_id() == anchor_id {
        receipt.retrieval_anchor().clone()
    } else if let Some(record) = receipt
        .repository_provenance_attachment()
        .anchor()
        .filter(|record| record.anchor_id() == anchor_id)
    {
        record.clone()
    } else {
        return Err(ObservationStoreError::RetrievalAnchorCollision);
    };
    record
        .validate()
        .map_err(ObservationStoreError::RetrievalAnchorContract)?;
    if receipt.observation().scope() != owner || record.owner() != owner {
        return Err(ObservationStoreError::RetrievalAnchorOwnerMismatch);
    }
    if record.projection_generation() != receipt.projection_generation() {
        return Err(ObservationStoreError::RetrievalAnchorProjectionGenerationMismatch);
    }
    Ok(Some(record))
}

/// Current position of the observation projection stream, defaulting to zero
/// before the first projection commits.
async fn read_projection_checkpoint_sequence(conn: &Connection) -> ObservationStoreResult<u64> {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| storage("read evidence anchor projection checkpoint", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read evidence anchor projection checkpoint", error))?
    else {
        return Ok(0);
    };
    decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read evidence anchor projection checkpoint", error))?,
        "read evidence anchor projection checkpoint",
    )
}

/// The observation store projects a single ordered observation stream, so the
/// resolver reports its current stream position under exactly the shard keys
/// the anchor's frozen watermark claims; shards the anchor never froze are
/// never claimed, and an empty frozen watermark stays exact.
fn observed_anchor_watermark(frozen: &VectorWatermark, observed_sequence: u64) -> VectorWatermark {
    let mut components = std::collections::BTreeMap::new();
    for shard in frozen.components.keys() {
        components.insert(shard.clone(), observed_sequence);
    }
    VectorWatermark { components }
}

async fn read_projection_status(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ObservationStoreResult<ObservationProjectionStatus> {
    let mut rows = conn
        .query(
            "SELECT EXISTS(
                SELECT 1 FROM projection_queue WHERE observation_id = ?1
             )",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read observation projection status", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read observation projection status", error))?
        .ok_or_else(|| {
            storage_message(
                "read observation projection status",
                "projection status query returned no row",
            )
        })?;
    match row
        .get::<i64>(0)
        .map_err(|error| storage("read observation projection status", error))?
    {
        0 => Ok(ObservationProjectionStatus::NotQueued),
        _ => Ok(ObservationProjectionStatus::Queued),
    }
}

impl GlobalDb {
    pub(crate) async fn get_observation_result(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ObservationStoreResult<Option<StoredObservation>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage("begin observation read snapshot", error))?;
        let Some(receipt) = read_by_observation_id(&snapshot, observation_id).await? else {
            return Ok(None);
        };
        let projection_status = read_projection_status(&snapshot, observation_id).await?;
        Ok(Some(StoredObservation::from_commit_receipt(
            receipt,
            projection_status,
        )))
    }

    /// Resolve an immutable observation-owned anchor without exposing the
    /// database handle to fact-materialization callers.
    pub(crate) async fn resolve_observation_evidence_anchor(
        &self,
        owner: &ObservationScopeV1,
        anchor_id: &RetrievalAnchorId,
    ) -> ObservationStoreResult<Option<RetrievalAnchorRecordV2>> {
        anchor_id
            .validate()
            .map_err(ObservationStoreError::RetrievalAnchorContract)?;
        owner
            .validate()
            .map_err(|_| ObservationStoreError::RetrievalAnchorOwnerMismatch)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage("begin evidence anchor read snapshot", error))?;
        resolve_owner_bound_anchor_record(&snapshot, owner, anchor_id).await
    }

    /// Resolve an observation-owned anchor into its typed store observation:
    /// the retained record with the store's current projection watermark, or a
    /// safe absent/ambiguous binding signal. Conflicting bindings never
    /// present a record, and a missing binding never errors.
    pub(crate) async fn resolve_observation_evidence_anchor_report(
        &self,
        owner: &ObservationScopeV1,
        anchor_id: &RetrievalAnchorId,
    ) -> ObservationStoreResult<ObservedEvidenceAnchorResolution> {
        anchor_id
            .validate()
            .map_err(ObservationStoreError::RetrievalAnchorContract)?;
        owner
            .validate()
            .map_err(|_| ObservationStoreError::RetrievalAnchorOwnerMismatch)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage("begin evidence anchor report snapshot", error))?;
        let record = match resolve_owner_bound_anchor_record(&snapshot, owner, anchor_id).await {
            Ok(record) => record,
            Err(ObservationStoreError::RetrievalAnchorCollision) => {
                return Ok(ObservedEvidenceAnchorResolution::Ambiguous);
            }
            Err(error) => return Err(error),
        };
        let Some(record) = record else {
            return Ok(ObservedEvidenceAnchorResolution::Unavailable);
        };
        let checkpoint = read_projection_checkpoint_sequence(&snapshot).await?;
        Ok(ObservedEvidenceAnchorResolution::Resolved {
            observed_watermark: observed_anchor_watermark(
                record.projection_watermark(),
                checkpoint,
            ),
            record: Box::new(record),
        })
    }

    pub(crate) async fn replay_observations_result(
        &self,
        request: ObservationReplayRequest,
    ) -> ObservationStoreResult<Vec<StoredObservation>> {
        let after_sequence = i64::try_from(request.after_sequence()).map_err(|_| {
            storage_message(
                "replay observations",
                "observation replay sequence exceeds SQLite integer range",
            )
        })?;
        let limit = i64::try_from(request.limit()).map_err(|_| {
            storage_message(
                "replay observations",
                "observation replay limit exceeds SQLite integer range",
            )
        })?;
        let mut rows = self
            .conn
            .query(
                "SELECT observations.sequence, observations.observation_json,
                        observations.committed_cursor_json, anchor.anchor_json,
                        anchor.projection_generation, repository.availability_json,
                        repository.capture_json, repository_anchor.anchor_json,
                        EXISTS(
                            SELECT 1 FROM projection_queue
                            WHERE projection_queue.observation_id = observations.observation_id
                        )
                 FROM observations
                 JOIN observation_retrieval_anchors AS binding
                   ON binding.observation_id = observations.observation_id
                 JOIN retrieval_anchors AS anchor ON anchor.anchor_id = binding.anchor_id
                 JOIN observation_repository_provenance AS repository
                   ON repository.observation_id = observations.observation_id
                 LEFT JOIN retrieval_anchors AS repository_anchor
                   ON repository_anchor.anchor_id = repository.retrieval_anchor_id
                 WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2",
                params![after_sequence, limit],
            )
            .await
            .map_err(|error| storage("replay observations", error))?;
        let mut observations = Vec::with_capacity(request.limit());
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("replay observations", error))?
        {
            let sequence = decode_sequence(
                row.get::<i64>(0)
                    .map_err(|error| storage("replay observations", error))?,
                "replay observations",
            )?;
            let observation_json = row
                .get::<String>(1)
                .map_err(|error| storage("replay observations", error))?;
            let committed_cursor_json = row
                .get::<String>(2)
                .map_err(|error| storage("replay observations", error))?;
            let anchor_json = row
                .get::<String>(3)
                .map_err(|error| storage("replay observations", error))?;
            let projection_generation = row
                .get::<String>(4)
                .map_err(|error| storage("replay observations", error))?;
            let repository_availability_json = row
                .get::<String>(5)
                .map_err(|error| storage("replay observations", error))?;
            let repository_capture_json = row
                .get::<Option<String>>(6)
                .map_err(|error| storage("replay observations", error))?;
            let repository_anchor_json = row
                .get::<Option<String>>(7)
                .map_err(|error| storage("replay observations", error))?;
            let projection_status = match row
                .get::<i64>(8)
                .map_err(|error| storage("replay observations", error))?
            {
                0 => ObservationProjectionStatus::NotQueued,
                _ => ObservationProjectionStatus::Queued,
            };
            let receipt = ObservationCommitReceipt::new(
                sequence,
                decode(&observation_json, "decode replayed observation")?,
                decode(&committed_cursor_json, "decode replayed observation cursor")?,
                decode(&anchor_json, "decode replayed observation anchor")?,
                ProjectionGenerationId::new(projection_generation)
                    .map_err(ObservationStoreError::RetrievalAnchorContract)?,
            )?
            .with_repository_provenance_attachment(
                decode_repository_provenance_attachment(
                    &repository_availability_json,
                    repository_capture_json.as_deref(),
                    repository_anchor_json.as_deref(),
                    "decode replayed repository provenance",
                )?,
            )?;
            observations.push(StoredObservation::from_commit_receipt(
                receipt,
                projection_status,
            ));
        }
        Ok(observations)
    }
}
