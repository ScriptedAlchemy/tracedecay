use tracedecay_store::{
    GraphPublicationInputDigestV1, GraphPublicationReplayV1, GraphRecoveredGenerationDigestV1,
    SemanticVectorBatchOutputDigest, SemanticVectorCheckpointDigest,
    SemanticVectorGraphBatchDigest, SemanticVectorStageBatchKey, SemanticVectorStageBatchReceipt,
    SemanticVectorStagePlan,
};

use crate::generation::recovered_generation_digest_from_database;
use crate::generation::{
    GraphGenerationReplaySource, checked_decode_replay_source, validate_metadata_binding,
};
use crate::lease::GenerationLocator;
use crate::limits::{
    MAX_SEMANTIC_VECTOR_GRAPH_BATCH_CANONICAL_BYTES, MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
    MAX_VERIFIED_GENERATION_ENTITIES, MAX_VERIFIED_GENERATION_RELATIONS,
    require_generation_capacity,
};
use crate::runtime::{GraphBatchPlan, PreparedGraphBatch};
use crate::state::{
    latest_projection, load_entity, load_relation, projection_node_counts, publication,
};
use crate::{
    GraphBudgetKind, GraphCommit, GraphDb, GraphDbError, GraphGenerationDependency,
    GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProjectionIdentity, GraphWriteBatch, SourceGeneration, mutation,
};

#[path = "generation_staging_runtime/native_contract.rs"]
mod native_contract;

impl GraphDb {
    pub(crate) fn apply_staged_generation_batch(
        &self,
        plan: &SemanticVectorStagePlan,
        receipt: &SemanticVectorStageBatchReceipt,
        mut batch: GraphWriteBatch,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        check()?;
        plan.validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        receipt
            .validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        if receipt.key.stage != plan.key {
            return Err(GraphDbError::Conflict);
        }
        let locator = generation_locator(plan)?;
        let physical_namespace = locator.physical_namespace()?;
        if batch.namespace.as_str() != plan.key.projection.namespace.as_str()
            || batch.projection.as_str() != plan.key.projection.projection.as_str()
            || batch.source_generation.as_str() != plan.source_generation.as_str()
        {
            return Err(GraphDbError::Conflict);
        }
        require_staged_batch_mutation_count(batch.mutations.len())?;
        native_contract::validate_semantic_native_batch(plan, receipt, &batch)?;
        let logical_output_digest = batch.semantic_vector_output_digest()?;
        require_receipt_output_digest(&logical_output_digest, &receipt.output_digest)?;
        batch.namespace = physical_namespace;
        let physical_output_digest = batch.semantic_vector_output_digest()?;
        let batch_digest = physical_output_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "canonical semantic vector graph batch digest is not sha256".to_owned(),
            })?
            .to_owned();
        let idempotency_key = batch_idempotency_key(&receipt.key)?;

        let _snapshot_gate = self.inner.snapshot_gate.write();
        self.require_staged_generation_writable(&locator)?;
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if let Some(existing) = publication(database, &batch.namespace, &idempotency_key)? {
            if existing.digest == batch_digest
                && existing.input_digest == receipt.receipt_digest.as_str()
            {
                return Ok(existing.commit);
            }
            return Err(GraphDbError::Conflict);
        }
        let (entity_count, relation_count) =
            projection_node_counts(database, &batch.namespace, &batch.projection)?;
        let mut new_entities = 0usize;
        let mut new_relations = 0usize;
        for mutation in &batch.mutations {
            check()?;
            match mutation {
                GraphMutation::UpsertEntity(entity)
                    if load_entity(database, &batch.namespace, &entity.identity)?.is_none() =>
                {
                    new_entities = new_entities.checked_add(1).ok_or_else(|| {
                        GraphDbError::budget_exhausted_count(
                            GraphBudgetKind::Capacity,
                            MAX_VERIFIED_GENERATION_ENTITIES,
                        )
                    })?;
                }
                GraphMutation::UpsertRelation(relation)
                    if load_relation(database, &batch.namespace, &relation.identity)?.is_none() =>
                {
                    new_relations = new_relations.checked_add(1).ok_or_else(|| {
                        GraphDbError::budget_exhausted_count(
                            GraphBudgetKind::Capacity,
                            MAX_VERIFIED_GENERATION_RELATIONS,
                        )
                    })?;
                }
                GraphMutation::UpsertEntity(_)
                | GraphMutation::DeleteEntity(_)
                | GraphMutation::UpsertRelation(_)
                | GraphMutation::DeleteRelation(_) => {}
            }
        }
        require_generation_capacity(
            "entities",
            entity_count,
            new_entities,
            MAX_VERIFIED_GENERATION_ENTITIES,
        )?;
        require_generation_capacity(
            "relations",
            relation_count,
            new_relations,
            MAX_VERIFIED_GENERATION_RELATIONS,
        )?;
        if receipt.key.ordinal > 0 {
            let prior_key = SemanticVectorStageBatchKey {
                stage: receipt.key.stage.clone(),
                ordinal: receipt.key.ordinal - 1,
            };
            let prior_idempotency = batch_idempotency_key(&prior_key)?;
            if publication(database, &batch.namespace, &prior_idempotency)?.is_none() {
                return Err(GraphDbError::unavailable(
                    "semantic vector graph batch predecessor is not applied",
                ));
            }
        }
        check()?;
        let mut state = self.state_write_guard()?;
        let commit = self.apply_locked(
            database,
            &mut state,
            batch,
            mutation::CommitMetadata {
                digest: batch_digest.clone(),
                generation_dependency_digest: None,
                publication_record: Some((
                    idempotency_key,
                    batch_digest,
                    receipt.receipt_digest.as_str().to_owned(),
                )),
            },
            &mutation::RelationEndpointNamespaces::new(),
            check,
        )?;
        Ok(commit)
    }

    pub(crate) fn staged_generation_batch_publication_digest(
        &self,
        plan: &SemanticVectorStagePlan,
        receipt: &SemanticVectorStageBatchReceipt,
    ) -> Result<SemanticVectorGraphBatchDigest, GraphDbError> {
        plan.validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        receipt
            .validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        if receipt.key.stage != plan.key {
            return Err(GraphDbError::Conflict);
        }
        let physical_namespace = generation_locator(plan)?.physical_namespace()?;
        let idempotency_key = batch_idempotency_key(&receipt.key)?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let publication = publication(database, &physical_namespace, &idempotency_key)?
            .ok_or_else(|| GraphDbError::ResetRequired {
                message: "semantic vector native batch publication is missing".to_owned(),
            })?;
        if publication.commit.digest != publication.digest
            || publication.commit.source_generation.as_str() != plan.source_generation.as_str()
        {
            return Err(GraphDbError::Conflict);
        }
        staged_publication_digest(
            &publication.digest,
            &publication.input_digest,
            &receipt.receipt_digest,
        )
    }

    pub(crate) fn prepare_publication_from_staged_native(
        &self,
        plan: &SemanticVectorStagePlan,
        checkpoint: &SemanticVectorCheckpointDigest,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        check()?;
        plan.validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let locator = generation_locator(plan)?;
        let physical_namespace = locator.physical_namespace()?;
        let projection = locator.projection.projection.clone();
        let dependency = stage_dependency(plan)?;
        let input_digest = finalization_input_digest(plan, checkpoint)?;
        let idempotency_key = finalization_idempotency_key(plan, checkpoint)?;
        let existing_input_digest = input_digest.clone();

        // The shared gated-batch choreography keeps the staged rows stable
        // (writers queue) while snapshot readers proceed: only the empty
        // finalization apply takes the exclusive claim, never the
        // recovered-digest stream.
        let replay = self.run_gated_batch(
            check,
            |database| {
                self.require_staged_generation_writable(&locator)?;
                let current = latest_projection(database, &physical_namespace, &projection)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "semantic vector staged native generation is missing".to_owned(),
                    })?
                    .commit;
                if current.source_generation.as_str() != plan.source_generation.as_str() {
                    return Err(GraphDbError::Conflict);
                }
                let manifest = GraphGenerationManifest::new_checked(
                    locator.projection.clone(),
                    locator.generation.clone(),
                    SourceGeneration::new(plan.source_generation.as_str())?,
                    current.watermark.clone(),
                    vec![dependency],
                    Vec::new(),
                    Vec::new(),
                    check,
                )?;
                let dependency_digest = manifest.dependency_closure_digest(check)?;
                let batch = GraphWriteBatch::new_canonical_checked(
                    physical_namespace.clone(),
                    projection.clone(),
                    manifest.source_generation.clone(),
                    manifest.watermark.clone(),
                    Vec::new(),
                    check,
                )?;
                let digest = batch.canonical_digest_checked(check)?;
                if let Some(existing) =
                    publication(database, &physical_namespace, &idempotency_key)?
                {
                    if existing.input_digest != existing_input_digest.as_str()
                        || existing.digest != digest
                    {
                        return Err(GraphDbError::Conflict);
                    }
                    return Ok(GraphBatchPlan::Settled(
                        existing.commit,
                        (manifest, dependency_digest),
                    ));
                }
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata {
                            digest: digest.clone(),
                            generation_dependency_digest: Some(dependency_digest.clone()),
                            publication_record: Some((
                                idempotency_key.clone(),
                                digest,
                                existing_input_digest.as_str().to_owned(),
                            )),
                        },
                        endpoint_namespaces: mutation::RelationEndpointNamespaces::new(),
                    },
                    (manifest, dependency_digest),
                ))
            },
            |database, _commit, (manifest, dependency_digest)| {
                let finalized = latest_projection(database, &physical_namespace, &projection)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "semantic vector finalized native generation is missing"
                            .to_owned(),
                    })?
                    .commit;
                if finalized.source_generation != manifest.source_generation
                    || finalized.watermark != manifest.watermark
                    || finalized.generation_dependency_digest.as_ref() != Some(&dependency_digest)
                {
                    return Err(GraphDbError::Conflict);
                }
                let recovered = GraphRecoveredGenerationDigestV1::new(format!(
                    "sha256:{}",
                    recovered_generation_digest_from_database(database, &manifest, check)?
                ))
                .map_err(|error| GraphDbError::Corrupt {
                    message: error.to_string(),
                })?;
                manifest.relational_semantic_vector_replay_with_recovered_digest(
                    plan,
                    GraphIdempotencyKey::new(plan.publication_key.idempotency_key.as_str())?,
                    input_digest,
                    recovered,
                    check,
                )
            },
        )?;
        validate_stage_publication_replay(plan, checkpoint, &replay, check)?;
        Ok(replay)
    }

    pub(crate) fn reserve_staged_generation_retirement(
        &self,
        plan: &SemanticVectorStagePlan,
    ) -> Result<(), GraphDbError> {
        let locator = generation_locator(plan)?;
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let mut state = self.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        if state.collected.contains(&locator) || state.retiring.contains(&locator) {
            return Ok(());
        }
        if state.retains(&locator) {
            return Err(GraphDbError::Conflict);
        }
        state.retiring.insert(locator);
        Ok(())
    }

    pub(crate) fn clear_staged_generation_retirement(
        &self,
        plan: &SemanticVectorStagePlan,
    ) -> Result<(), GraphDbError> {
        let locator = generation_locator(plan)?;
        let mut state = self.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        state.retiring.remove(&locator);
        Ok(())
    }

    pub(crate) fn delete_cancelled_staged_generation(
        &self,
        plan: &SemanticVectorStagePlan,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let locator = generation_locator(plan)?;
        self.delete_generation_contents(&locator, check)
    }

    fn require_staged_generation_writable(
        &self,
        locator: &GenerationLocator,
    ) -> Result<(), GraphDbError> {
        let state = self.inner.verified_generations.read().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        if state.retiring.contains(locator) || state.collected.contains(locator) {
            return Err(GraphDbError::Conflict);
        }
        Ok(())
    }
}

fn stage_dependency(
    plan: &SemanticVectorStagePlan,
) -> Result<GraphGenerationDependency, GraphDbError> {
    Ok(GraphGenerationDependency::new(
        GraphProjectionIdentity::new(
            GraphNamespace::new(
                plan.source_dependency
                    .generation
                    .projection
                    .namespace
                    .as_str(),
            )?,
            GraphProjectionId::new(
                plan.source_dependency
                    .generation
                    .projection
                    .projection
                    .as_str(),
            )?,
        ),
        GraphGenerationId::new(plan.source_dependency.generation.generation.as_str())?,
        GraphIdempotencyKey::new(plan.source_dependency.idempotency_key.as_str())?,
    ))
}

fn finalization_input_digest(
    plan: &SemanticVectorStagePlan,
    checkpoint: &SemanticVectorCheckpointDigest,
) -> Result<GraphPublicationInputDigestV1, GraphDbError> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.semantic-vector-native-finalization.v1",
        &plan.key,
        checkpoint,
        &plan.publication_key,
        &plan.source_dependency,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    GraphPublicationInputDigestV1::new(digest.as_str())
        .map_err(|error| GraphDbError::invalid(error.to_string()))
}

fn finalization_idempotency_key(
    plan: &SemanticVectorStagePlan,
    checkpoint: &SemanticVectorCheckpointDigest,
) -> Result<GraphIdempotencyKey, GraphDbError> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.semantic-vector-native-finalization-idempotency.v1",
        &plan.key,
        checkpoint,
        &plan.publication_key,
        &plan.source_dependency,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    GraphIdempotencyKey::new(format!("semantic-vector-finalize:{}", digest.as_str()))
}

fn staged_publication_digest(
    native_digest: &str,
    native_input_digest: &str,
    receipt_digest: &tracedecay_store::SemanticVectorBatchReceiptDigest,
) -> Result<SemanticVectorGraphBatchDigest, GraphDbError> {
    if native_input_digest != receipt_digest.as_str() {
        return Err(GraphDbError::Conflict);
    }
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.semantic-vector-physical-batch-publication.v1",
        native_digest,
        receipt_digest,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    SemanticVectorGraphBatchDigest::new(digest.as_str()).map_err(|error| GraphDbError::Corrupt {
        message: error.to_string(),
    })
}

pub(crate) fn validate_stage_publication_replay(
    plan: &SemanticVectorStagePlan,
    checkpoint: &SemanticVectorCheckpointDigest,
    replay: &GraphPublicationReplayV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check()?;
    replay
        .validate()
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    if replay.key != plan.publication_key
        || replay.expected_prior_head != plan.expected_prior_verified_head
        || replay.input_digest != finalization_input_digest(plan, checkpoint)?
    {
        return Err(GraphDbError::Conflict);
    }
    let source = checked_decode_replay_source(&replay.canonical_replay_source, check)?;
    let GraphGenerationReplaySource::SemanticVectorGeneration(source) = source else {
        return Err(GraphDbError::Conflict);
    };
    if source.semantic_generation_id != plan.semantic_generation_id
        || source.base_generation != plan.base_generation
        || source.metadata.source_generation.as_str() != plan.source_generation.as_str()
        || source.metadata.dependencies != vec![stage_dependency(plan)?]
    {
        return Err(GraphDbError::Conflict);
    }
    let manifest = GraphGenerationManifest::new_checked(
        source.metadata.projection,
        source.metadata.generation,
        source.metadata.source_generation,
        source.metadata.watermark,
        source.metadata.dependencies,
        Vec::new(),
        Vec::new(),
        check,
    )?;
    validate_metadata_binding(replay, &manifest, false, check)
}

impl GraphWriteBatch {
    pub fn semantic_vector_output_digest(
        &mut self,
    ) -> Result<SemanticVectorBatchOutputDigest, GraphDbError> {
        let digest =
            self.validate_and_digest_with_limit(MAX_SEMANTIC_VECTOR_GRAPH_BATCH_CANONICAL_BYTES)?;
        SemanticVectorBatchOutputDigest::new(format!("sha256:{digest}"))
            .map_err(|error| GraphDbError::invalid(error.to_string()))
    }
}

fn require_staged_batch_mutation_count(count: usize) -> Result<(), GraphDbError> {
    if count > MAX_VERIFIED_GENERATION_BATCH_MUTATIONS {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Mutation,
            MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
        ));
    }
    Ok(())
}

fn require_receipt_output_digest(
    actual: &SemanticVectorBatchOutputDigest,
    recorded: &SemanticVectorBatchOutputDigest,
) -> Result<(), GraphDbError> {
    if actual != recorded {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn generation_locator(plan: &SemanticVectorStagePlan) -> Result<GenerationLocator, GraphDbError> {
    Ok(GenerationLocator::new(
        GraphProjectionIdentity::new(
            GraphNamespace::new(plan.key.projection.namespace.as_str())?,
            GraphProjectionId::new(plan.key.projection.projection.as_str())?,
        ),
        GraphGenerationId::new(plan.publication_key.generation.as_str())?,
    ))
}

fn batch_idempotency_key(
    key: &SemanticVectorStageBatchKey,
) -> Result<GraphIdempotencyKey, GraphDbError> {
    GraphIdempotencyKey::new(format!(
        "semantic-vector-stage:{}:{}:{}",
        key.stage.build_id.as_str(),
        key.stage.plan_digest.as_str(),
        key.ordinal
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use super::{
        require_receipt_output_digest, require_staged_batch_mutation_count,
        staged_publication_digest,
    };
    use crate::{
        GraphBudgetKind, GraphDbError, GraphNamespace, GraphProjectionId, GraphWatermark,
        GraphWriteBatch, MAX_VERIFIED_GENERATION_BATCH_MUTATIONS, NeverCancelled, SourceGeneration,
    };
    use tracedecay_store::{
        SemanticVectorBatchOutputDigest, SemanticVectorBatchReceiptDigest,
        SemanticVectorGraphBatchDigest,
    };

    #[test]
    fn production_width_vector_page_stays_inside_named_graph_budgets() {
        use crate::{
            GraphEntity, GraphEntityId, GraphLabel, GraphMutation, GraphProperty,
            GraphPropertyName, GraphVector, VectorMetric,
        };

        let page = 512usize;
        let dimension = 768usize;
        let values = vec![0.125_f32; dimension];
        let mut mutations = Vec::with_capacity(page);
        for index in 0..page {
            mutations.push(GraphMutation::UpsertEntity(
                GraphEntity::new(
                    GraphEntityId::new(format!("vector-{index}")).unwrap(),
                    BTreeSet::from([
                        GraphLabel::new("semantic-vector-generation-vector-v1").unwrap()
                    ]),
                    BTreeMap::from([(
                        GraphPropertyName::new("vector").unwrap(),
                        GraphProperty::Vector(
                            GraphVector::new(values.clone(), dimension, VectorMetric::Cosine)
                                .unwrap(),
                        ),
                    )]),
                )
                .expect("512 x 768-d vector entities must stay inside property budgets"),
            ));
        }
        assert_eq!(
            require_staged_batch_mutation_count(mutations.len()),
            Ok(()),
            "a production-width page must stay inside the mutation budget"
        );
        let mut batch = GraphWriteBatch::new(
            GraphNamespace::new("logical.semantic").unwrap(),
            GraphProjectionId::new("projection.semantic").unwrap(),
            SourceGeneration::new("source.semantic").unwrap(),
            GraphWatermark::new("watermark.semantic").unwrap(),
            mutations,
            Arc::new(NeverCancelled),
        )
        .expect("production-width page must construct");
        batch
            .semantic_vector_output_digest()
            .expect("production-width page digest must stay inside the 32 MiB write budget");
    }

    #[test]
    fn semantic_vector_stage_rejects_more_than_one_bounded_native_batch() {
        assert_eq!(
            require_staged_batch_mutation_count(MAX_VERIFIED_GENERATION_BATCH_MUTATIONS + 1),
            Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Mutation,
                MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
            ))
        );
    }

    #[test]
    fn semantic_vector_stage_binds_receipt_to_exact_native_batch_digest() {
        let actual =
            SemanticVectorBatchOutputDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let foreign =
            SemanticVectorBatchOutputDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();

        assert_eq!(
            require_receipt_output_digest(&actual, &foreign),
            Err(GraphDbError::Conflict)
        );
        assert_eq!(require_receipt_output_digest(&actual, &actual), Ok(()));
    }

    #[test]
    fn semantic_vector_stage_uses_logical_receipt_and_physical_replay_digests() {
        let mut batch = GraphWriteBatch::new(
            GraphNamespace::new("logical.semantic").unwrap(),
            GraphProjectionId::new("projection.semantic").unwrap(),
            SourceGeneration::new("source.semantic").unwrap(),
            GraphWatermark::new("watermark.semantic").unwrap(),
            vec![],
            Arc::new(NeverCancelled),
        )
        .unwrap();
        let logical = batch.semantic_vector_output_digest().unwrap();
        assert_eq!(require_receipt_output_digest(&logical, &logical), Ok(()));

        batch.namespace = GraphNamespace::new("physical.semantic.generation").unwrap();
        let physical = batch.semantic_vector_output_digest().unwrap();
        assert_ne!(logical, physical);
        assert_eq!(
            SemanticVectorGraphBatchDigest::new(physical.as_str())
                .unwrap()
                .as_str(),
            physical.as_str()
        );
    }

    #[test]
    fn stage_settlement_uses_native_digest_bound_to_the_exact_receipt() {
        let receipt =
            SemanticVectorBatchReceiptDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let settled =
            staged_publication_digest(&"b".repeat(64), receipt.as_str(), &receipt).unwrap();
        let expected = tracedecay_domain::canonical_sha256(&(
            "tracedecay.semantic-vector-physical-batch-publication.v1",
            "b".repeat(64),
            &receipt,
        ))
        .unwrap();
        assert_eq!(settled.as_str(), expected.as_str());
        assert_eq!(
            staged_publication_digest(
                &"b".repeat(64),
                &format!("sha256:{}", "c".repeat(64)),
                &receipt
            ),
            Err(GraphDbError::Conflict)
        );
    }
}
