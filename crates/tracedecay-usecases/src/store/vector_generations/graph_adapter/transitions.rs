use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_domain::{VectorGenerationIdV1, canonical_sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphMutation, GraphRelation, GraphRelationId,
    GraphWatermark,
};

use super::super::{
    MAX_STATE_CAS_RETRIES, PhysicalVectorBytePoolV1, PreparedVectorGenerationV1, PublishedStateV1,
    VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, VectorGenerationBuildIdV1, VectorGenerationPlanV1,
    VectorGenerationPublicationV1, VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1,
    VectorProjectionCheckpointV1, generation_identity_digest, validate_plan,
};
use super::GraphVectorGenerationStoreV1;
use super::native_records::{
    NativeGraphStateV1, ScopedBuildRecordsV1, ScopedGenerationRecordsV1, encode_state,
    read_build_records, read_cataloged_generation_records, read_state_metadata,
};
use super::persistence::{check_cancelled, map_graph_error, storage_error};

const TRANSITION_DIGEST_DOMAIN: &str = "tracedecay.semantic-vector.record-transition.v1";

impl GraphVectorGenerationStoreV1 {
    pub(super) fn begin_generation_records(
        &self,
        plan: VectorGenerationPlanV1,
        rebuild: bool,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        validate_plan(&plan)?;
        let build_id = VectorGenerationBuildIdV1(
            canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &plan))
                .map_err(storage_error)?,
        );
        for _ in 0..MAX_STATE_CAS_RETRIES {
            check_cancelled(cancellation.as_ref())?;
            let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
            let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
            let existing = read_build_records(&snapshot, &build_id, Arc::clone(&cancellation))?;
            if !rebuild
                && let Some(existing) = &existing
                && existing.staged.plan == plan
            {
                return Ok(build_id);
            }
            let mut generations = Vec::new();
            push_required_generation(
                &mut generations,
                &snapshot,
                plan.base_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            push_required_generation(
                &mut generations,
                &snapshot,
                metadata.active_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let before = transition_state(
                existing.as_ref(),
                generations.iter(),
                metadata.active_generation.clone(),
            )?;
            let mut after = before.clone();
            let result = if rebuild {
                after.rebuild_generation(plan.clone())
            } else {
                after.begin_generation(plan.clone())
            }?;
            drop(snapshot);
            match self.publish_transition(
                &before,
                &after,
                metadata.revision,
                metadata.watermark,
                plan.source_generation.to_string(),
                Arc::clone(&cancellation),
            ) {
                Ok(_) => return Ok(result),
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    pub(super) fn cancel_generation_records(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            check_cancelled(cancellation.as_ref())?;
            let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
            let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
            let Some(build) = read_build_records(&snapshot, build_id, Arc::clone(&cancellation))?
            else {
                return Ok(false);
            };
            let mut generations = Vec::new();
            push_required_generation(
                &mut generations,
                &snapshot,
                metadata.active_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let before = transition_state(
                Some(&build),
                generations.iter(),
                metadata.active_generation.clone(),
            )?;
            let mut after = before.clone();
            let source_generation = build.staged.plan.source_generation.to_string();
            let removed = after.cancel_generation(build_id);
            drop(snapshot);
            match self.publish_transition(
                &before,
                &after,
                metadata.revision,
                metadata.watermark,
                source_generation,
                Arc::clone(&cancellation),
            ) {
                Ok(_) => return Ok(removed),
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    pub(super) fn commit_batch_records(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            check_cancelled(cancellation.as_ref())?;
            let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
            let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
            let build = read_build_records(&snapshot, build_id, Arc::clone(&cancellation))?
                .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
            let mut generations = Vec::new();
            push_required_generation(
                &mut generations,
                &snapshot,
                build.staged.plan.base_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            push_required_generation(
                &mut generations,
                &snapshot,
                metadata.active_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let before = transition_state(
                Some(&build),
                generations.iter(),
                metadata.active_generation.clone(),
            )?;
            let mut after = before.clone();
            let checkpoint = after.commit_batch_ref(build_id, expected_checkpoint, &prepared)?;
            let source_generation = build.staged.plan.source_generation.to_string();
            drop(snapshot);
            match self.publish_transition(
                &before,
                &after,
                metadata.revision,
                metadata.watermark,
                source_generation,
                Arc::clone(&cancellation),
            ) {
                Ok(_) => return Ok(checkpoint),
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    pub(super) fn publish_generation_records(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active: Option<&VectorGenerationIdV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            check_cancelled(cancellation.as_ref())?;
            let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
            let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
            if metadata.active_generation.as_ref() != expected_active {
                return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
            }
            let build = read_build_records(&snapshot, build_id, Arc::clone(&cancellation))?
                .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
            let mut generations = Vec::new();
            push_required_generation(
                &mut generations,
                &snapshot,
                build.staged.plan.base_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let target_id = VectorGenerationIdV1::new(generation_identity_digest(
                &build.staged.plan,
                &build.staged.vectors,
                &build.staged.tombstones,
            )?);
            push_optional_generation(
                &mut generations,
                &snapshot,
                &target_id,
                Arc::clone(&cancellation),
            )?;
            push_required_generation(
                &mut generations,
                &snapshot,
                metadata.active_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let before = transition_state(
                Some(&build),
                generations.iter(),
                metadata.active_generation.clone(),
            )?;
            let mut after = before.clone();
            let publication = after.publish_generation(build_id, expected_active)?;
            let source_generation = build.staged.plan.source_generation.to_string();
            drop(snapshot);
            match self.publish_transition(
                &before,
                &after,
                metadata.revision,
                metadata.watermark,
                source_generation,
                Arc::clone(&cancellation),
            ) {
                Ok(_) => return Ok(publication),
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    pub(super) fn activate_generation_records(
        &self,
        generation_id: &VectorGenerationIdV1,
        expected_active: Option<&VectorGenerationIdV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            check_cancelled(cancellation.as_ref())?;
            let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
            let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
            let mut generations = Vec::new();
            push_optional_generation(
                &mut generations,
                &snapshot,
                generation_id,
                Arc::clone(&cancellation),
            )?;
            push_required_generation(
                &mut generations,
                &snapshot,
                metadata.active_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let before = transition_state(
                None,
                generations.iter(),
                metadata.active_generation.clone(),
            )?;
            let mut after = before.clone();
            let publication = after.activate_generation(generation_id, expected_active)?;
            let source_generation = generations
                .iter()
                .find(|records| records.generation.generation_id() == generation_id)
                .map(|records| records.generation.source_generation().to_string())
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
            drop(snapshot);
            match self.publish_transition(
                &before,
                &after,
                metadata.revision,
                metadata.watermark,
                source_generation,
                Arc::clone(&cancellation),
            ) {
                Ok(_) => return Ok(publication),
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    pub(super) fn deactivate_generation_records(
        &self,
        expected_active: Option<&VectorGenerationIdV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            check_cancelled(cancellation.as_ref())?;
            let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
            let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
            let mut generations = Vec::new();
            push_required_generation(
                &mut generations,
                &snapshot,
                metadata.active_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let before = transition_state(
                None,
                generations.iter(),
                metadata.active_generation.clone(),
            )?;
            let mut after = before.clone();
            after.deactivate_generation(expected_active)?;
            let source_generation = generations
                .iter()
                .find(|records| {
                    Some(records.generation.generation_id()) == metadata.active_generation.as_ref()
                })
                .map(|records| records.generation.source_generation().to_string())
                .unwrap_or_else(|| "semantic-vector-unpublished".to_owned());
            drop(snapshot);
            match self.publish_transition(
                &before,
                &after,
                metadata.revision,
                metadata.watermark,
                source_generation,
                Arc::clone(&cancellation),
            ) {
                Ok(_) => return Ok(()),
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    fn publish_transition(
        &self,
        before: &VectorGenerationStateMachineV1,
        after: &VectorGenerationStateMachineV1,
        revision: u64,
        expected_watermark: GraphWatermark,
        source_generation: String,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, VectorGenerationStoreErrorV1> {
        let next_revision = revision.checked_add(1).ok_or_else(|| {
            VectorGenerationStoreErrorV1::Corrupt(
                "semantic vector graph revision overflowed".to_owned(),
            )
        })?;
        let before = encode_state(before, revision)?;
        let after = encode_state(after, next_revision)?;
        let mutations = native_delta(before, after);
        let input_digest = canonical_sha256(&(
            TRANSITION_DIGEST_DOMAIN,
            &expected_watermark,
            next_revision,
            &mutations,
        ))
        .map_err(storage_error)?;
        self.publish_record_mutations(
            next_revision,
            expected_watermark,
            source_generation,
            input_digest,
            mutations,
            cancellation,
        )
    }
}

fn transition_state<'a>(
    build: Option<&ScopedBuildRecordsV1>,
    generations: impl Iterator<Item = &'a ScopedGenerationRecordsV1>,
    active_generation: Option<VectorGenerationIdV1>,
) -> Result<VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1> {
    let staged = build
        .map(|build| -> Result<_, VectorGenerationStoreErrorV1> {
            Ok(BTreeMap::from([(
                VectorGenerationBuildIdV1(
                    canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &build.staged.plan))
                        .map_err(storage_error)?,
                ),
                build.staged.clone(),
            )]))
        })
        .transpose()?
        .unwrap_or_default();
    let generations = generations
        .map(|records| {
            (
                records.generation.generation_id().clone(),
                records.generation.clone(),
            )
        })
        .collect();
    let mut state = VectorGenerationStateMachineV1 {
        staged,
        published: PublishedStateV1 {
            generations,
            active_generation,
            physical_vectors: BTreeMap::new(),
            physical_vector_bindings: BTreeMap::new(),
        },
        physical_vector_pool: PhysicalVectorBytePoolV1::default(),
        fail_before_publication_swap: false,
    };
    state.ensure_physical_reuse_index()?;
    Ok(state)
}

fn push_required_generation(
    generations: &mut Vec<ScopedGenerationRecordsV1>,
    snapshot: &tracedecay_graph_db::GraphSnapshot,
    generation_id: Option<&VectorGenerationIdV1>,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let Some(generation_id) = generation_id else {
        return Ok(());
    };
    if generations
        .iter()
        .any(|records| records.generation.generation_id() == generation_id)
    {
        return Ok(());
    }
    let records = read_cataloged_generation_records(snapshot, generation_id, cancellation)?
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
    generations.push(records);
    Ok(())
}

fn push_optional_generation(
    generations: &mut Vec<ScopedGenerationRecordsV1>,
    snapshot: &tracedecay_graph_db::GraphSnapshot,
    generation_id: &VectorGenerationIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if generations
        .iter()
        .any(|records| records.generation.generation_id() == generation_id)
    {
        return Ok(());
    }
    if let Some(records) = read_cataloged_generation_records(snapshot, generation_id, cancellation)?
    {
        generations.push(records);
    }
    Ok(())
}

fn native_delta(before: NativeGraphStateV1, after: NativeGraphStateV1) -> Vec<GraphMutation> {
    let before_entities = entity_map(before.entities);
    let after_entities = entity_map(after.entities);
    let before_relations = relation_map(before.relations);
    let after_relations = relation_map(after.relations);
    let mut mutations = before_relations
        .keys()
        .filter(|identity| !after_relations.contains_key(*identity))
        .cloned()
        .map(GraphMutation::DeleteRelation)
        .collect::<Vec<_>>();
    mutations.extend(
        before_entities
            .keys()
            .filter(|identity| !after_entities.contains_key(*identity))
            .cloned()
            .map(GraphMutation::DeleteEntity),
    );
    mutations.extend(after_entities.into_iter().filter_map(|(identity, entity)| {
        (before_entities.get(&identity) != Some(&entity))
            .then_some(GraphMutation::UpsertEntity(entity))
    }));
    mutations.extend(
        after_relations
            .into_iter()
            .filter_map(|(identity, relation)| {
                (before_relations.get(&identity) != Some(&relation))
                    .then_some(GraphMutation::UpsertRelation(relation))
            }),
    );
    mutations
}

fn entity_map(entities: Vec<GraphEntity>) -> BTreeMap<GraphEntityId, GraphEntity> {
    entities
        .into_iter()
        .map(|entity| (entity.identity.clone(), entity))
        .collect()
}

fn relation_map(relations: Vec<GraphRelation>) -> BTreeMap<GraphRelationId, GraphRelation> {
    relations
        .into_iter()
        .map(|relation| (relation.identity.clone(), relation))
        .collect()
}
