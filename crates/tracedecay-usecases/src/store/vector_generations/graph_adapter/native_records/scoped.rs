use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{
    CodeChunkProjectionReceiptV1, CodeSearchChunkId, ProjectionBatchReceiptV1,
    ProjectionOperationV1, ProjectionOutcomeV1, VectorGenerationIdV1, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphRelation, GraphRelationId,
    GraphTraversalDirection, TraversalRequest,
};

use super::super::super::{
    CommittedVectorBatchV1, ExternalV1, PreparedBatchesV1, ProjectedChunkVectorV1,
    PublishedVectorGenerationV1, StagedVectorGenerationV1, VECTOR_GENERATION_BUILD_DIGEST_DOMAIN,
    VectorGenerationBuildIdV1, VectorGenerationPlanV1, VectorGenerationStoreErrorV1,
    VectorRowMapV1, validate_plan, validate_vector_row,
};
use super::super::persistence::map_graph_error;
use super::VectorProjectionCheckpointV1;
use super::{
    BASE_GENERATION, BASE_KIND, BATCH_COUNT, BUILD_BATCH_LABEL, BUILD_ID, BUILD_LABEL,
    BUILD_MEMBER_LABEL, CHECKPOINT, CHUNK_ID, CONTAINS_KIND, EMBEDDING_KEY, EXPECTED_COUNT,
    GENERATION_ID, GENERATION_LABEL, GENERATION_RECEIPT_LABEL, GENERATION_TOMBSTONE_LABEL,
    GENERATION_VECTOR_LABEL, MANIFEST_DIGEST, ORDINAL, PREPARED_DIGEST, PRIOR_DIGEST,
    RECEIPT_COUNT, REQUEST_DIGEST, ROW_COUNT, SOURCE_GENERATION, SOURCE_MANIFEST,
    STAGED_TOMBSTONE_LABEL, STAGED_VECTOR_LABEL, TARGET_PROJECTION, TOMBSTONE_COUNT, VECTOR_BYTES,
    VECTOR_COUNT, build_entity_id, build_id, content_digest, decode_vector, digest,
    generation_entity_id, generation_id, optional_bytes, optional_generation, parse_id,
    relation_kind, require_labels, required_bytes, required_string, required_u64, rows_with_owner,
};

// Each logical row can contribute one entity and one relation; builds also
// retain one expected-member record and, in the worst case, one batch record.
const MAX_BUILD_SCOPE_RECORDS: usize = super::super::MAX_RESIDENT_VECTOR_ROWS * 6 + 4;
const MAX_GENERATION_SCOPE_RECORDS: usize = super::super::MAX_RESIDENT_VECTOR_ROWS * 4 + 4;

pub(crate) struct ScopedBuildRecordsV1 {
    pub staged: StagedVectorGenerationV1,
}

pub(crate) struct ScopedGenerationRecordsV1 {
    pub generation: PublishedVectorGenerationV1,
    pub vector_bytes: u64,
    pub entities: BTreeMap<GraphEntityId, GraphEntity>,
}

pub(crate) fn read_build_records(
    snapshot: &super::super::snapshot::SemanticVectorVerifiedRead,
    build: &VectorGenerationBuildIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<ScopedBuildRecordsV1>, VectorGenerationStoreErrorV1> {
    let Some((entities, relations)) = read_scope(
        snapshot,
        build_entity_id(build)?,
        MAX_BUILD_SCOPE_RECORDS,
        cancellation,
    )?
    else {
        return Ok(None);
    };
    let owner = entities
        .get(&build_entity_id(build)?)
        .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
    require_labels(owner, [BUILD_LABEL])?;
    let parsed = build_id(required_string(owner, BUILD_ID)?)?;
    if &parsed != build {
        return Err(corrupt("semantic vector build identity is inconsistent"));
    }
    let plan = VectorGenerationPlanV1 {
        target_projection_key: required_bytes(owner, TARGET_PROJECTION)?,
        source_generation: parse_id(required_string(owner, SOURCE_GENERATION)?)?,
        source_manifest_digest: digest(required_string(owner, SOURCE_MANIFEST)?)?,
        expected_chunk_ids: {
            // Member rows are set-semantic and surface in graph-entity order;
            // the canonical plan form is strictly ascending, so reconstruction
            // restores that order. Duplicates still fail canonical validation.
            let mut chunk_ids =
                rows_with_owner(&entities, BUILD_MEMBER_LABEL, BUILD_ID, build.0.as_str())?
                    .into_iter()
                    .map(|member| parse_id(required_string(member, CHUNK_ID)?))
                    .collect::<Result<Vec<_>, _>>()?;
            chunk_ids.sort();
            chunk_ids.into()
        },
        base_generation: optional_generation(owner, BASE_GENERATION)?,
    };
    validate_plan(&plan).map_err(|error| {
        corrupt(format!(
            "semantic vector build failed canonical plan validation: {error}"
        ))
    })?;
    let canonical_build = VectorGenerationBuildIdV1(
        canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &plan))
            .map_err(super::storage_error)?,
    );
    if &canonical_build != build {
        return Err(corrupt(
            "semantic vector build id does not match its canonical plan",
        ));
    }
    let vectors = rows_with_owner(&entities, STAGED_VECTOR_LABEL, BUILD_ID, build.0.as_str())?
        .into_iter()
        .map(|row| {
            decode_vector(
                row,
                &plan.target_projection_key,
                &plan.source_generation,
                &plan.source_manifest_digest,
            )
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let tombstones = rows_with_owner(
        &entities,
        STAGED_TOMBSTONE_LABEL,
        BUILD_ID,
        build.0.as_str(),
    )?
    .into_iter()
    .map(|row| {
        Ok((
            parse_id(required_string(row, CHUNK_ID)?)?,
            content_digest(required_string(row, PRIOR_DIGEST)?)?,
        ))
    })
    .collect::<Result<BTreeMap<_, _>, VectorGenerationStoreErrorV1>>()?;
    let mut batches = rows_with_owner(&entities, BUILD_BATCH_LABEL, BUILD_ID, build.0.as_str())?
        .into_iter()
        .map(|row| {
            Ok((
                required_u64(row, ORDINAL)?,
                CommittedVectorBatchV1 {
                    request_digest: digest(required_string(row, REQUEST_DIGEST)?)?,
                    prepared_digest: digest(required_string(row, PREPARED_DIGEST)?)?,
                    receipt: super::support::required_generation_receipt(row)?,
                },
            ))
        })
        .collect::<Result<Vec<_>, VectorGenerationStoreErrorV1>>()?;
    batches.sort_by_key(|(ordinal, _)| *ordinal);
    let batches = batches
        .into_iter()
        .enumerate()
        .map(|(expected, (ordinal, batch))| {
            if ordinal != expected as u64 {
                Err(corrupt(
                    "semantic vector build batch ordinals are non-canonical",
                ))
            } else {
                Ok(batch)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let effects = batches
        .iter()
        .flat_map(|batch| batch.receipt.receipts.iter())
        .map(|receipt| receipt.chunk_id.clone())
        .collect::<BTreeSet<_>>();
    let effect_count = batches.iter().try_fold(0_usize, |count, batch| {
        count
            .checked_add(batch.receipt.receipts.len())
            .ok_or_else(|| corrupt("semantic vector build effect count overflowed"))
    })?;
    if effects.len() != effect_count {
        return Err(corrupt(
            "semantic vector build contains duplicate committed chunk effects",
        ));
    }
    let checkpoint: VectorProjectionCheckpointV1 = required_bytes(owner, CHECKPOINT)?;
    if checkpoint.target_projection_key != plan.target_projection_key
        || checkpoint.source_generation != plan.source_generation
        || checkpoint.source_manifest_digest != plan.source_manifest_digest
        || checkpoint.completed_batches
            != u64::try_from(batches.len()).map_err(super::storage_error)?
        || checkpoint.last_request_digest.as_ref()
            != batches.last().map(|batch| &batch.request_digest)
        || checkpoint.last_publication_digest.as_ref()
            != batches
                .last()
                .map(|batch| &batch.receipt.publication_digest)
    {
        return Err(corrupt("semantic vector build checkpoint is inconsistent"));
    }
    // The plan's expected chunk IDs enumerate the vectors the finished
    // generation must carry (publication requires exact equality); deletions
    // are tombstones outside that set, so effects may land on either.
    let expected_chunks = plan.expected_chunk_ids.iter().collect::<BTreeSet<_>>();
    if vectors.keys().any(|chunk| !expected_chunks.contains(chunk))
        || effects
            .iter()
            .any(|chunk| !expected_chunks.contains(chunk) && !tombstones.contains_key(chunk))
        || vectors.keys().any(|chunk| tombstones.contains_key(chunk))
    {
        return Err(corrupt(
            "semantic vector build contains an out-of-plan or conflicting chunk effect",
        ));
    }
    let embedding_key = optional_bytes(owner, EMBEDDING_KEY)?;
    if let Some(embedding_key) = &embedding_key {
        for vector in vectors.values() {
            validate_vector_row(&plan, embedding_key, vector).map_err(|error| {
                corrupt(format!(
                    "semantic vector staged row failed canonical validation: {error}"
                ))
            })?;
        }
    } else if !vectors.is_empty() {
        return Err(corrupt(
            "semantic vector build has vectors without an admitted embedding key",
        ));
    }
    require_count(owner, EXPECTED_COUNT, plan.expected_chunk_ids.len())?;
    require_count(owner, VECTOR_COUNT, vectors.len())?;
    require_count(owner, TOMBSTONE_COUNT, tombstones.len())?;
    require_count(owner, BATCH_COUNT, batches.len())?;
    let expected_base_entity = plan
        .base_generation
        .as_ref()
        .map(generation_entity_id)
        .transpose()?;
    let base_relations = relations
        .values()
        .filter(|relation| relation.kind.as_str() == BASE_KIND)
        .collect::<Vec<_>>();
    if base_relations.len() != usize::from(expected_base_entity.is_some())
        || base_relations
            .iter()
            .any(|relation| expected_base_entity.as_ref() != Some(&relation.to))
    {
        return Err(corrupt(
            "semantic vector build base relation is inconsistent",
        ));
    }
    let contained_records = plan
        .expected_chunk_ids
        .len()
        .checked_add(vectors.len())
        .and_then(|count| count.checked_add(tombstones.len()))
        .and_then(|count| count.checked_add(batches.len()))
        .ok_or_else(|| corrupt("semantic vector build relation count overflowed"))?;
    let expected_relations = contained_records
        .checked_add(usize::from(plan.base_generation.is_some()))
        .ok_or_else(|| corrupt("semantic vector build relation count overflowed"))?;
    // The base relation points into the prior generation's scope; its target
    // entity is intentionally not hydrated here, so only contained children
    // plus the owner are expected in the entity set.
    if relations.len() != expected_relations
        || entities.len()
            != contained_records
                .checked_add(1)
                .ok_or_else(|| corrupt("semantic vector build record count overflowed"))?
    {
        return Err(corrupt(
            "semantic vector build scope contains unknown or missing records",
        ));
    }
    Ok(Some(ScopedBuildRecordsV1 {
        staged: StagedVectorGenerationV1 {
            plan,
            embedding_key,
            vectors: ExternalV1::from(VectorRowMapV1(vectors)),
            tombstones: ExternalV1::from(tombstones),
            batches: ExternalV1::from(PreparedBatchesV1(batches)),
            committed_chunk_effects: ExternalV1::from(effects),
            checkpoint,
        },
    }))
}

pub(crate) fn read_generation_records(
    snapshot: &super::super::snapshot::SemanticVectorVerifiedRead,
    generation: &VectorGenerationIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<ScopedGenerationRecordsV1>, VectorGenerationStoreErrorV1> {
    let Some((entities, relations)) = read_scope(
        snapshot,
        generation_entity_id(generation)?,
        MAX_GENERATION_SCOPE_RECORDS,
        cancellation,
    )?
    else {
        return Ok(None);
    };
    let owner = entities
        .get(&generation_entity_id(generation)?)
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
    require_labels(owner, [GENERATION_LABEL])?;
    if generation_id(required_string(owner, GENERATION_ID)?)? != *generation {
        return Err(corrupt(
            "semantic vector generation identity is inconsistent",
        ));
    }
    let projection_key = required_bytes(owner, TARGET_PROJECTION)?;
    let source_generation = parse_id(required_string(owner, SOURCE_GENERATION)?)?;
    let source_manifest_digest = digest(required_string(owner, SOURCE_MANIFEST)?)?;
    let vectors = rows_with_owner(
        &entities,
        GENERATION_VECTOR_LABEL,
        GENERATION_ID,
        generation.as_digest().as_str(),
    )?
    .into_iter()
    .map(|row| {
        decode_vector(
            row,
            &projection_key,
            &source_generation,
            &source_manifest_digest,
        )
    })
    .collect::<Result<BTreeMap<_, _>, _>>()?;
    let tombstone_digests = rows_with_owner(
        &entities,
        GENERATION_TOMBSTONE_LABEL,
        GENERATION_ID,
        generation.as_digest().as_str(),
    )?
    .into_iter()
    .map(|row| {
        Ok((
            parse_id(required_string(row, CHUNK_ID)?)?,
            content_digest(required_string(row, PRIOR_DIGEST)?)?,
        ))
    })
    .collect::<Result<BTreeMap<_, _>, VectorGenerationStoreErrorV1>>()?;
    let mut receipts = rows_with_owner(
        &entities,
        GENERATION_RECEIPT_LABEL,
        GENERATION_ID,
        generation.as_digest().as_str(),
    )?
    .into_iter()
    .map(|row| {
        Ok((
            required_u64(row, ORDINAL)?,
            super::support::required_generation_receipt(row)?,
        ))
    })
    .collect::<Result<Vec<_>, VectorGenerationStoreErrorV1>>()?;
    receipts.sort_by_key(|(ordinal, _)| *ordinal);
    let mut receipts = receipts
        .into_iter()
        .enumerate()
        .map(|(expected, (ordinal, receipt))| {
            if ordinal != expected as u64 {
                Err(corrupt(
                    "semantic vector generation receipt ordinals are non-canonical",
                ))
            } else {
                Ok(receipt)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    attach_reused_receipts(&mut receipts, &vectors)?;
    require_count(owner, ROW_COUNT, vectors.len())?;
    let measured_vector_bytes = vectors.values().try_fold(0_u64, |total, vector| {
        total
            .checked_add(
                u64::try_from(vector.values.len())
                    .map_err(super::storage_error)?
                    .checked_mul(4)
                    .ok_or_else(|| corrupt("semantic vector byte count overflowed"))?,
            )
            .ok_or_else(|| corrupt("semantic vector byte count overflowed"))
    })?;
    if required_u64(owner, VECTOR_BYTES)? != measured_vector_bytes {
        return Err(corrupt(
            "semantic vector generation byte count is inconsistent",
        ));
    }
    require_count(owner, TOMBSTONE_COUNT, tombstone_digests.len())?;
    require_count(owner, RECEIPT_COUNT, receipts.len())?;
    let base_generation = optional_generation(owner, BASE_GENERATION)?;
    let base_relations = relations
        .values()
        .filter(|relation| relation.kind.as_str() == BASE_KIND)
        .collect::<Vec<_>>();
    if !base_relations.is_empty() {
        return Err(corrupt(
            "immutable semantic vector generation links to a foreign physical generation",
        ));
    }
    let contained_records = vectors
        .len()
        .checked_add(tombstone_digests.len())
        .and_then(|count| count.checked_add(receipts.len()))
        .ok_or_else(|| corrupt("semantic vector generation relation count overflowed"))?;
    if relations.len() != contained_records
        || entities.len()
            != contained_records
                .checked_add(1)
                .ok_or_else(|| corrupt("semantic vector generation record count overflowed"))?
    {
        return Err(corrupt(
            "semantic vector generation scope contains unknown or missing records",
        ));
    }
    let generation_record = PublishedVectorGenerationV1 {
        generation_id: generation.clone(),
        projection_key,
        source_generation,
        source_manifest_digest,
        base_generation,
        embedding_key: required_bytes(owner, EMBEDDING_KEY)?,
        vectors: ExternalV1::from(VectorRowMapV1(vectors)),
        tombstones: ExternalV1::from(tombstone_digests.keys().cloned().collect::<Vec<_>>()),
        tombstone_digests: ExternalV1::from(tombstone_digests),
        receipts: ExternalV1::from(receipts),
        checkpoint: required_bytes(owner, CHECKPOINT)?,
        manifest_digest: digest(required_string(owner, MANIFEST_DIGEST)?)?,
    };
    generation_record.validate_persisted().map_err(|error| {
        corrupt(format!(
            "semantic vector generation failed canonical validation: {error}"
        ))
    })?;
    Ok(Some(ScopedGenerationRecordsV1 {
        generation: generation_record,
        vector_bytes: measured_vector_bytes,
        entities,
    }))
}

#[allow(clippy::type_complexity)]
fn read_scope(
    snapshot: &super::super::snapshot::SemanticVectorVerifiedRead,
    owner: GraphEntityId,
    max_records: usize,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<
    Option<(
        BTreeMap<GraphEntityId, GraphEntity>,
        BTreeMap<GraphRelationId, GraphRelation>,
    )>,
    VectorGenerationStoreErrorV1,
> {
    let namespace = snapshot.projection().namespace.clone();
    let Some(owner_row) = snapshot
        .entity(&namespace, &owner, Arc::clone(&cancellation))
        .map_err(map_graph_error)?
    else {
        return Ok(None);
    };
    let traversal = snapshot
        .traverse(TraversalRequest {
            namespace: namespace.clone(),
            start: owner.clone(),
            relation_kinds: BTreeSet::from([
                relation_kind(CONTAINS_KIND)?,
                relation_kind(BASE_KIND)?,
            ]),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 1,
            max_visits: max_records,
            max_results: max_records,
            cancellation: Arc::clone(&cancellation),
        })
        .map_err(map_graph_error)?;
    let mut entities = BTreeMap::from([(owner, owner_row)]);
    let mut relations = BTreeMap::new();
    for visit in traversal.visits {
        let Some(relation_id) = visit.via_relation else {
            continue;
        };
        let relation = snapshot
            .relation(&namespace, &relation_id, Arc::clone(&cancellation))
            .map_err(map_graph_error)?
            .ok_or_else(|| corrupt("semantic vector scope relation is missing"))?;
        if relation.kind.as_str() == CONTAINS_KIND {
            let child = snapshot
                .entity(&namespace, &visit.entity, Arc::clone(&cancellation))
                .map_err(map_graph_error)?
                .ok_or_else(|| corrupt("semantic vector scope child is missing"))?;
            entities.insert(child.identity.clone(), child);
        }
        relations.insert(relation.identity.clone(), relation);
    }
    if entities
        .len()
        .checked_add(relations.len())
        .is_none_or(|count| count > max_records)
    {
        return Err(VectorGenerationStoreErrorV1::Unavailable(
            "semantic vector record scope exceeds its transition ceiling".to_owned(),
        ));
    }
    Ok(Some((entities, relations)))
}

fn attach_reused_receipts(
    receipts: &mut [ProjectionBatchReceiptV1],
    vectors: &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if receipts.iter().any(|batch| {
        batch
            .receipts
            .iter()
            .any(|receipt| receipt.operation == ProjectionOperationV1::Reused)
    }) {
        return Ok(());
    }
    let mut named = BTreeSet::new();
    let mut reused_batch = None;
    for (index, batch) in receipts.iter().enumerate() {
        if batch.reused_count > 0 {
            if reused_batch.replace(index).is_some() {
                return Err(corrupt(
                    "published generation names reused receipts on more than one batch",
                ));
            }
        }
        for receipt in &batch.receipts {
            if !named.insert(receipt.chunk_id.clone()) {
                return Err(corrupt(
                    "published generation receipts name a chunk more than once",
                ));
            }
        }
    }
    let mut unused = vectors
        .keys()
        .filter(|chunk_id| !named.contains(*chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    unused.sort();
    let Some(index) = reused_batch else {
        if unused.is_empty() {
            return Ok(());
        }
        return Err(corrupt(
            "published generation has vectors that no receipt names",
        ));
    };
    let batch = &mut receipts[index];
    if unused.len() as u64 != batch.reused_count {
        return Err(corrupt(
            "published reused receipt count does not match unnamed vectors",
        ));
    }
    let synthesized = unused
        .into_iter()
        .map(|chunk_id| {
            let vector = vectors
                .get(&chunk_id)
                .ok_or_else(|| corrupt("published reused receipt is missing its vector"))?;
            Ok(CodeChunkProjectionReceiptV1 {
                projection_key: batch.target_projection_key.clone(),
                request_digest: batch.request_digest.clone(),
                prior_generation: None,
                source_generation: batch.source_generation.clone(),
                source_manifest_digest: batch.source_manifest_digest.clone(),
                chunk_id,
                prior_chunk_digest: Some(vector.chunk_digest.clone()),
                current_chunk_digest: Some(vector.chunk_digest.clone()),
                operation: ProjectionOperationV1::Reused,
                outcome: ProjectionOutcomeV1::Reused,
                output_digest: None,
            })
        })
        .collect::<Result<Vec<_>, VectorGenerationStoreErrorV1>>()?;
    batch.receipts.extend(synthesized);
    Ok(())
}

fn require_count(
    owner: &GraphEntity,
    property: &str,
    actual: usize,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if required_u64(owner, property)? == u64::try_from(actual).map_err(super::storage_error)? {
        Ok(())
    } else {
        Err(corrupt(format!(
            "semantic vector record count {property} is inconsistent"
        )))
    }
}

fn corrupt(message: impl Into<String>) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Corrupt(message.into())
}
