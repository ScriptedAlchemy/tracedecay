mod backfill;
mod codec;
mod persist;
pub mod retention;
mod schema;

pub use backfill::OBSERVATION_PROVENANCE_SCHEMA_MIGRATION;
pub use schema::OBSERVATION_ANCHOR_SCHEMA_MIGRATION;
pub(super) use schema::ensure_observation_schema;

use tracedecay_domain::{
    AnchorSourceGenerationV2, CanonicalObservationIdV1, ObservationScopeV1,
    ObservationSourceGenerationV1, RetrievalAnchorId, RetrievalAnchorRecordV2,
    RetrievalAnchorTargetV2, VectorWatermark,
};
use tracedecay_store::{
    ObservationStoreError, ObservationStoreResult, ObservedEvidenceAnchorResolution,
    SESSION_MESSAGE_PROJECTOR_VERSION,
};

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

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
    validate_exact_observation_provenance(
        record.target(),
        record.source_generation(),
        record.source_observations(),
        receipt.observation().observation_id(),
        receipt.observation().identity().generation(),
    )?;
    Ok(Some(record))
}

fn validate_exact_observation_provenance(
    target: &RetrievalAnchorTargetV2,
    source_generation: &AnchorSourceGenerationV2,
    source_observations: &[CanonicalObservationIdV1],
    observation_id: &CanonicalObservationIdV1,
    observation_generation: ObservationSourceGenerationV1,
) -> ObservationStoreResult<()> {
    let RetrievalAnchorTargetV2::ExactObservation(target_observation_id) = target else {
        return Ok(());
    };
    if target_observation_id != observation_id {
        return Err(ObservationStoreError::RetrievalAnchorObservationMismatch);
    }
    if source_generation != &AnchorSourceGenerationV2::Observation(observation_generation) {
        return Err(ObservationStoreError::RetrievalAnchorSourceGenerationMismatch);
    }
    if source_observations != std::slice::from_ref(observation_id) {
        return Err(ObservationStoreError::RetrievalAnchorSourceLineageMismatch);
    }
    Ok(())
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

impl super::RegisteredGlobalDb {
    pub async fn resolve_observation_evidence_anchor(
        &self,
        owner: &ObservationScopeV1,
        anchor_id: &RetrievalAnchorId,
    ) -> ObservationStoreResult<Option<RetrievalAnchorRecordV2>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage("open evidence anchor resolution snapshot", error))?;
        resolve_owner_bound_anchor_record(&snapshot, owner, anchor_id).await
    }

    pub async fn resolve_observation_evidence_anchor_report(
        &self,
        owner: &ObservationScopeV1,
        anchor_id: &RetrievalAnchorId,
    ) -> ObservationStoreResult<ObservedEvidenceAnchorResolution> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage("open evidence anchor report snapshot", error))?;
        let record = match resolve_owner_bound_anchor_record(&snapshot, owner, anchor_id).await {
            Ok(Some(record)) => record,
            Ok(None) => return Ok(ObservedEvidenceAnchorResolution::Unavailable),
            Err(ObservationStoreError::RetrievalAnchorCollision) => {
                return Ok(ObservedEvidenceAnchorResolution::Ambiguous);
            }
            Err(error) => return Err(error),
        };
        let observed_sequence = read_projection_checkpoint_sequence(&snapshot).await?;
        Ok(ObservedEvidenceAnchorResolution::Resolved {
            observed_watermark: observed_anchor_watermark(
                record.projection_watermark(),
                observed_sequence,
            ),
            record: Box::new(record),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{CommitId, RepositoryId};

    fn observation_id(byte: &str) -> CanonicalObservationIdV1 {
        CanonicalObservationIdV1::new(format!("sha256:{}", byte.repeat(64))).unwrap()
    }

    #[test]
    fn exact_observation_resolution_rechecks_generation_and_ordered_lineage() {
        let canonical_id = observation_id("a");
        let other_id = observation_id("b");
        let generation = ObservationSourceGenerationV1::new(7).unwrap();
        let target = RetrievalAnchorTargetV2::ExactObservation(canonical_id.clone());
        let source_generation = AnchorSourceGenerationV2::Observation(generation);

        assert!(
            validate_exact_observation_provenance(
                &target,
                &source_generation,
                std::slice::from_ref(&canonical_id),
                &canonical_id,
                generation,
            )
            .is_ok()
        );
        assert!(matches!(
            validate_exact_observation_provenance(
                &target,
                &AnchorSourceGenerationV2::Observation(
                    ObservationSourceGenerationV1::new(8).unwrap(),
                ),
                std::slice::from_ref(&canonical_id),
                &canonical_id,
                generation,
            ),
            Err(ObservationStoreError::RetrievalAnchorSourceGenerationMismatch)
        ));
        assert!(matches!(
            validate_exact_observation_provenance(
                &target,
                &source_generation,
                &[canonical_id.clone(), other_id],
                &canonical_id,
                generation,
            ),
            Err(ObservationStoreError::RetrievalAnchorSourceLineageMismatch)
        ));
        assert!(matches!(
            validate_exact_observation_provenance(
                &RetrievalAnchorTargetV2::ExactObservation(observation_id("c")),
                &source_generation,
                std::slice::from_ref(&canonical_id),
                &canonical_id,
                generation,
            ),
            Err(ObservationStoreError::RetrievalAnchorObservationMismatch)
        ));
    }

    #[test]
    fn non_observation_anchor_keeps_its_own_provenance_contract() {
        let target = RetrievalAnchorTargetV2::ExactRepositoryCommit {
            repository_id: RepositoryId::new("repository.fixture").unwrap(),
            commit_id: CommitId::new("commit.fixture").unwrap(),
        };
        assert!(
            validate_exact_observation_provenance(
                &target,
                &AnchorSourceGenerationV2::Unknown,
                &[],
                &observation_id("a"),
                ObservationSourceGenerationV1::new(7).unwrap(),
            )
            .is_ok()
        );
    }
}
