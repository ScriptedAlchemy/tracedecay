mod codec;
mod persist;
mod provenance_backfill;
pub mod retention;
mod schema;

pub(super) use schema::ensure_observation_schema;

use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationScopeV1, RetrievalAnchorId, RetrievalAnchorRecordV2,
    VectorWatermark,
};
use tracedecay_store::{
    ObservationProjectionStatus, ObservationStoreError, ObservationStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION,
};

use crate::db::engine::{QueryExecutor, params};

use codec::{decode_sequence, storage, storage_message};
use persist::read_by_observation_id;

async fn read_observation_id_for_retrieval_anchor(
    conn: &impl QueryExecutor,
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
    conn: &impl QueryExecutor,
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
async fn read_projection_checkpoint_sequence(
    conn: &impl QueryExecutor,
) -> ObservationStoreResult<u64> {
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
    conn: &impl QueryExecutor,
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
