use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::{VectorGenerationIdV1, canonical_sha256};
use tracedecay_graph_db::{GraphCancellation, GraphMutation, GraphWatermark};

use crate::semantic_runtime::SemanticRetainedVectorGenerationsV1;

use super::super::{MAX_STATE_CAS_RETRIES, VectorGenerationStoreErrorV1};
use super::GraphVectorGenerationStoreV1;
use super::native_records::{
    generation_catalog_relation_id, read_build_catalog, read_control_entity,
    read_generation_catalog, read_generation_records, read_state_metadata, set_control_revision,
};
use super::persistence::{check_cancelled, map_graph_error, storage_error};

const RECLAIM_DIGEST_DOMAIN: &str = "tracedecay.semantic-vector.record-reclaim.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VectorGenerationReclaimReceiptV1 {
    pub watermark: GraphWatermark,
    pub reclaimed_generation_ids: Vec<VectorGenerationIdV1>,
    pub rows: u64,
    pub vector_bytes: u64,
    pub remaining: u64,
}

impl GraphVectorGenerationStoreV1 {
    pub async fn reclaim_unretained_generations(
        &self,
        expected_active: Option<&VectorGenerationIdV1>,
        retained: &SemanticRetainedVectorGenerationsV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationReclaimReceiptV1, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            check_cancelled(cancellation.as_ref())?;
            let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
            let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
            if metadata.active_generation.as_ref() != expected_active {
                return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
            }
            let mut generations = read_generation_catalog(&snapshot, Arc::clone(&cancellation))?;
            generations.sort_by(|left, right| left.generation_id.cmp(&right.generation_id));
            let known = generations
                .iter()
                .map(|entry| entry.generation_id.clone())
                .collect::<BTreeSet<_>>();
            if known.len() != generations.len() {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector generation catalog contains duplicate identities".to_owned(),
                ));
            }
            for retained_generation in retained.generation_ids() {
                if !known.contains(retained_generation) {
                    return Err(VectorGenerationStoreErrorV1::ResetRequired(format!(
                        "retained semantic vector generation {retained_generation:?} is missing"
                    )));
                }
            }
            let builds = read_build_catalog(&snapshot, Arc::clone(&cancellation))?;
            let build_ids = builds
                .iter()
                .map(|entry| entry.build_id.clone())
                .collect::<BTreeSet<_>>();
            if build_ids.len() != builds.len() {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector build catalog contains duplicate identities".to_owned(),
                ));
            }
            let mut retained_generations = retained.generation_ids().clone();
            retained_generations.extend(metadata.active_generation.iter().cloned());
            retained_generations.extend(
                builds
                    .iter()
                    .filter_map(|entry| entry.base_generation.clone()),
            );
            if retained_generations
                .iter()
                .any(|generation| !known.contains(generation))
            {
                return Err(VectorGenerationStoreErrorV1::ResetRequired(
                    "semantic vector catalog contains a missing retained base generation"
                        .to_owned(),
                ));
            }
            let candidates = generations
                .iter()
                .filter(|entry| !retained_generations.contains(&entry.generation_id))
                .filter(|entry| {
                    !generations.iter().any(|descendant| {
                        descendant.base_generation.as_ref() == Some(&entry.generation_id)
                    })
                })
                .collect::<Vec<_>>();
            let Some(candidate) = candidates.first() else {
                return Ok(VectorGenerationReclaimReceiptV1 {
                    watermark: metadata.watermark,
                    reclaimed_generation_ids: Vec::new(),
                    rows: 0,
                    vector_bytes: 0,
                    remaining: 0,
                });
            };
            let records = read_generation_records(
                &snapshot,
                &candidate.generation_id,
                Arc::clone(&cancellation),
            )?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Corrupt(
                    "selected semantic vector reclaim generation is missing".to_owned(),
                )
            })?;
            if u64::try_from(records.generation.vectors().len()).map_err(storage_error)?
                != candidate.rows
                || records.vector_bytes != candidate.vector_bytes
                || records.generation.base_generation() != candidate.base_generation.as_ref()
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector reclaim measures are inconsistent".to_owned(),
                ));
            }
            let next_revision = metadata.revision.checked_add(1).ok_or_else(|| {
                VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector graph revision overflowed".to_owned(),
                )
            })?;
            let mut control = read_control_entity(&snapshot, Arc::clone(&cancellation))?;
            set_control_revision(&mut control, next_revision)?;
            let mut mutations = records
                .relations
                .keys()
                .cloned()
                .map(GraphMutation::DeleteRelation)
                .collect::<Vec<_>>();
            mutations.push(GraphMutation::DeleteRelation(
                generation_catalog_relation_id(&candidate.generation_id)?,
            ));
            mutations.extend(
                records
                    .entities
                    .keys()
                    .cloned()
                    .map(GraphMutation::DeleteEntity),
            );
            mutations.push(GraphMutation::UpsertEntity(control));
            let input_digest = canonical_sha256(&(
                RECLAIM_DIGEST_DOMAIN,
                &metadata.watermark,
                next_revision,
                &candidate.generation_id,
                candidate.rows,
                candidate.vector_bytes,
            ))
            .map_err(storage_error)?;
            let generation_id = candidate.generation_id.clone();
            let rows = candidate.rows;
            let vector_bytes = candidate.vector_bytes;
            let remaining =
                u64::try_from(candidates.len().saturating_sub(1)).map_err(storage_error)?;
            drop(snapshot);
            match self.publish_record_mutations(
                next_revision,
                metadata.watermark,
                "semantic-vector-reclaim".to_owned(),
                input_digest,
                mutations,
                Arc::clone(&cancellation),
            ) {
                Ok(watermark) => {
                    return Ok(VectorGenerationReclaimReceiptV1 {
                        watermark,
                        reclaimed_generation_ids: vec![generation_id],
                        rows,
                        vector_bytes,
                        remaining,
                    });
                }
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }
}
